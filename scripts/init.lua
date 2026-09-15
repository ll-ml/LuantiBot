local function get_player(name)
    if not name or name == "" then
        return nil
    end
    return minetest.get_player_by_name(name)
end

local function punch_target(attacker, target)
    local apos = attacker:get_pos()
    local tpos = target:get_pos()
    if not apos or not tpos then
        return false, "missing position"
    end
    local dir = vector.subtract(tpos, apos)
    if vector.length(dir) == 0 then
        dir = { x = 0, y = 0, z = 0 }
    else
        dir = vector.normalize(dir)
    end
    local wielded = attacker:get_wielded_item()
    local toolcaps = wielded and wielded:get_tool_capabilities() or nil
    if not toolcaps then
        toolcaps = {
            full_punch_interval = 1.0,
            max_drop_level = 0,
            damage_groups = { fleshy = 2 },
        }
    end
    target:punch(attacker, 1.0, toolcaps, dir)
    return true, "punched"
end

local function send_bot_json(name, tag, payload)
    if minetest.write_json then
        minetest.chat_send_player(name, tag .. " " .. minetest.write_json(payload))
    else
        minetest.chat_send_player(name, tag .. " " .. minetest.serialize(payload))
    end
end

local function command_result(name, tag, ok, status, message, details)
    local payload = details or {}
    payload.ok = ok
    payload.status = status
    send_bot_json(name, tag, payload)
    return ok, message or status
end

minetest.register_chatcommand("bot_attack", {
    params = "<player>",
    description = "Server-side punch a player as the caller",
    privs = { interact = true },
    func = function(name, param)
        local attacker = get_player(name)
        if not attacker then
            return command_result(name, "BOT_ATTACK", false, "no_attacker", "attacker not found")
        end
        local target_name = param:gsub("^%s+", ""):gsub("%s+$", "")
        if target_name == "" then
            return command_result(name, "BOT_ATTACK", false, "missing_target", "missing target name")
        end
        local target = get_player(target_name)
        if not target then
            return command_result(
                name, "BOT_ATTACK", false, "target_not_found", "target not found",
                { target = target_name }
            )
        end
        local ok, msg = punch_target(attacker, target)
        if ok then
            minetest.log("action", "[llm_bot] " .. name .. " punched " .. target_name)
        end
        return command_result(
            name, "BOT_ATTACK", ok, ok and "attacked" or "attack_failed", msg,
            { target = target_name }
        )
    end,
})

minetest.register_chatcommand("bot_punch", {
    params = "<player>",
    description = "Alias for /bot_attack",
    privs = { interact = true },
    func = function(name, param)
        return minetest.registered_chatcommands.bot_attack.func(name, param)
    end,
})

local function get_cardinal_facing(player)
    local dir = player:get_look_dir()
    if not dir then
        return "north"
    end
    local ax = math.abs(dir.x)
    local az = math.abs(dir.z)
    if ax > az then
        if dir.x >= 0 then
            return "east"
        end
        return "west"
    end
    if dir.z >= 0 then
        return "south"
    end
    return "north"
end

local function get_facing_offsets(facing)
    if facing == "north" then
        return {
            front = { x = 0, y = 0, z = -1 },
            back = { x = 0, y = 0, z = 1 },
            left = { x = -1, y = 0, z = 0 },
            right = { x = 1, y = 0, z = 0 },
        }
    elseif facing == "south" then
        return {
            front = { x = 0, y = 0, z = 1 },
            back = { x = 0, y = 0, z = -1 },
            left = { x = 1, y = 0, z = 0 },
            right = { x = -1, y = 0, z = 0 },
        }
    elseif facing == "east" then
        return {
            front = { x = 1, y = 0, z = 0 },
            back = { x = -1, y = 0, z = 0 },
            left = { x = 0, y = 0, z = -1 },
            right = { x = 0, y = 0, z = 1 },
        }
    end
    return {
        front = { x = -1, y = 0, z = 0 },
        back = { x = 1, y = 0, z = 0 },
        left = { x = 0, y = 0, z = 1 },
        right = { x = 0, y = 0, z = -1 },
    }
end

local function get_obstacles(node_pos, facing)
    local offsets = get_facing_offsets(facing)
    local obstacles = {}
    for key, offset in pairs(offsets) do
        local p = vector.add(node_pos, offset)
        local node = minetest.get_node_or_nil(p)
        obstacles[key] = node and node.name or "unknown"
    end
    return obstacles
end

local function is_hostile_entity(ent, ent_def)
    if not ent and not ent_def then
        return false
    end
    local mob_type = ent and ent.type or ent_def and ent_def.type
    if mob_type == "monster" or mob_type == "hostile" then
        return true
    end
    local groups = ent and ent.groups or ent_def and ent_def.groups
    if groups and ((groups.monster or 0) > 0 or (groups.hostile or 0) > 0) then
        return true
    end
    if ent and (ent.hostile == true or ent.attack_players == true) then
        return true
    end
    if ent_def and (ent_def.hostile == true or ent_def.attack_players == true) then
        return true
    end
    return false
end

local function is_mob_entity(ent, ent_def)
    if not ent and not ent_def then
        return false
    end
    if ent and ent.is_mob == true then
        return true
    end
    if ent_def and ent_def.is_mob == true then
        return true
    end
    local mob_type = ent and ent.type or ent_def and ent_def.type
    return mob_type == "animal" or mob_type == "npc"
        or mob_type == "monster" or mob_type == "hostile"
end

local function entity_category(ent, ent_def)
    return ent and ent.type or ent_def and ent_def.type or "unknown"
end

local PATH_RADIUS_MIN = 2
local PATH_RADIUS_MAX = 32
local PATH_NODE_CANDIDATE_LIMIT = 24
local PATH_ATTEMPT_LIMIT = 64
local PATH_WAYPOINT_LIMIT = 192
local HUNT_RADIUS_MAX = 32
local HUNT_ATTACK_RANGE = 4
local HUNT_MIN_REMAINING_ADULTS = 1
local HUNT_STRIKE_INTERVAL = 0.75
local HUNT_MAX_STRIKES = 8
local HUNT_EXPIRY_SECONDS = 45

local hunt_targets = {}
local next_hunt_id = 1

local function split_words(value)
    local words = {}
    for word in string.gmatch(value or "", "[^%s]+") do
        words[#words + 1] = word
    end
    return words
end

local function clamp_integer(value, default, minimum, maximum)
    local number = tonumber(value) or default
    return math.floor(math.max(minimum, math.min(number, maximum)))
end

local function position_array(pos)
    return { pos.x, pos.y, pos.z }
end

local function item_food_metadata(item_name)
    local food_group = minetest.get_item_group(item_name, "food")
    local food_points = minetest.get_item_group(item_name, "eatable")
    local def = minetest.registered_items[item_name]
    local saturation = def and tonumber(def._mcl_saturation) or nil
    return {
        food = food_group > 0 or food_points > 0,
        food_group = food_group,
        food_points = food_points,
        food_saturation = saturation,
    }
end

local function stack_observation(stack, include_wear)
    local item_name = stack:get_name()
    local food = item_food_metadata(item_name)
    local result = {
        name = item_name,
        count = stack:get_count(),
        food = food.food,
        food_group = food.food_group,
        food_points = food.food_points,
        food_saturation = food.food_saturation,
    }
    if include_wear then
        result.wear = stack:get_wear()
    end
    return result
end

local function normalize_drop(drop)
    local item_string = nil
    local chance = 1
    local minimum = 1
    local maximum = 1
    if type(drop) == "string" then
        item_string = drop
    elseif type(drop) == "table" then
        item_string = drop.name or drop[1]
        chance = tonumber(drop.chance) or chance
        minimum = tonumber(drop.min) or minimum
        maximum = tonumber(drop.max) or minimum
    end
    if type(item_string) ~= "string" or item_string == "" then
        return nil
    end
    local stack = ItemStack(item_string)
    if stack:is_empty() then
        return nil
    end
    local item_name = stack:get_name()
    local food = item_food_metadata(item_name)
    if not food.food then
        return nil
    end
    return {
        name = item_name,
        chance = math.max(1, math.floor(chance)),
        min = math.max(0, math.floor(minimum)),
        max = math.max(0, math.floor(maximum)),
        food_points = food.food_points,
        food_saturation = food.food_saturation,
    }
end

local function entity_food_drops(ent, ent_def)
    local drops = ent and ent.drops or ent_def and ent_def.drops
    local food_drops = {}
    if type(drops) ~= "table" then
        return food_drops
    end
    for _, drop in ipairs(drops) do
        local normalized = normalize_drop(drop)
        if normalized then
            food_drops[#food_drops + 1] = normalized
        end
    end
    return food_drops
end

local function entity_nametag(obj, ent)
    local nametag = ent and ent.nametag or ""
    if type(nametag) ~= "string" then
        nametag = ""
    end
    if nametag == "" and obj and type(obj.get_properties) == "function" then
        local ok, properties = pcall(obj.get_properties, obj)
        if ok and properties and type(properties.nametag) == "string" then
            nametag = properties.nametag
        end
    end
    return nametag
end

local function food_mob_metadata(obj, ent, ent_def)
    local category = entity_category(ent, ent_def)
    local spawn_class = ent and ent.spawn_class or ent_def and ent_def.spawn_class
    local passive = (ent and ent.passive == true)
        or (ent_def and ent_def.passive == true)
        or spawn_class == "passive"
    local child = ent and ent.child == true or false
    local tamed = ent and ent.tamed == true or false
    local owner = ent and ent.owner or ""
    if type(owner) ~= "string" then
        owner = ""
    end
    local nametag = entity_nametag(obj, ent)
    local persistent = ent and (ent.persistent == true or ent._persistent == true) or false
    local food_drops = entity_food_drops(ent, ent_def)
    local food_source = #food_drops > 0
    local hostile = is_hostile_entity(ent, ent_def)
    local safe_to_hunt = category == "animal" and passive and not hostile
        and not child and not tamed and owner == "" and nametag == ""
        and not persistent and food_source and obj:get_hp() > 0
    local blocked_reason = nil
    if category ~= "animal" then
        blocked_reason = "not_animal"
    elseif not passive or hostile then
        blocked_reason = "not_passive"
    elseif child then
        blocked_reason = "child"
    elseif tamed then
        blocked_reason = "tamed"
    elseif owner ~= "" then
        blocked_reason = "owned"
    elseif nametag ~= "" then
        blocked_reason = "named"
    elseif persistent then
        blocked_reason = "persistent"
    elseif not food_source then
        blocked_reason = "no_food_drops"
    elseif obj:get_hp() <= 0 then
        blocked_reason = "dead"
    end
    return {
        category = category,
        passive = passive,
        adult = not child,
        child = child,
        tamed = tamed,
        owned = owner ~= "",
        owner = owner,
        named = nametag ~= "",
        nametag = nametag,
        persistent = persistent,
        food_source = food_source,
        food_drops = food_drops,
        safe_to_hunt = safe_to_hunt,
        hunt_blocked_reason = blocked_reason,
    }
end

local function hunger_observation(player)
    if type(mcl_hunger) ~= "table"
            or type(mcl_hunger.get_hunger) ~= "function"
            or type(mcl_hunger.get_saturation) ~= "function" then
        return false, nil, nil
    end
    local hunger_ok, hunger = pcall(mcl_hunger.get_hunger, player)
    local saturation_ok, saturation = pcall(mcl_hunger.get_saturation, player)
    if not hunger_ok or not saturation_ok
            or type(hunger) ~= "number" or type(saturation) ~= "number" then
        return false, nil, nil
    end
    return true, hunger, saturation
end

local function pathfinder_available()
    if type(minetest.find_path) ~= "function" then
        return false
    end
    return not minetest.features or minetest.features.pathfinder_works ~= false
end

local function safe_empty_node(pos)
    local node = minetest.get_node_or_nil(pos)
    if not node or node.name == "ignore" then
        return false
    end
    local def = minetest.registered_nodes[node.name]
    if not def or def.walkable == true then
        return false
    end
    if def.liquidtype and def.liquidtype ~= "none" then
        return false
    end
    return (tonumber(def.damage_per_second) or 0) <= 0
end

local function safe_standing_position(pos)
    local foot = vector.round(pos)
    local head = { x = foot.x, y = foot.y + 1, z = foot.z }
    local floor_pos = { x = foot.x, y = foot.y - 1, z = foot.z }
    if not safe_empty_node(foot) or not safe_empty_node(head) then
        return false
    end
    local floor_node = minetest.get_node_or_nil(floor_pos)
    if not floor_node or floor_node.name == "ignore" then
        return false
    end
    local floor_def = minetest.registered_nodes[floor_node.name]
    if not floor_def or floor_def.walkable ~= true then
        return false
    end
    return (tonumber(floor_def.damage_per_second) or 0) <= 0
end

local function path_to(player, destination, search_distance)
    if not pathfinder_available() then
        return nil, "pathfinder_unavailable"
    end
    local start = vector.round(player:get_pos())
    local ok, path = pcall(
        minetest.find_path,
        start,
        vector.round(destination),
        search_distance,
        1,
        2,
        "A*_noprefetch"
    )
    if not ok or type(path) ~= "table" or #path == 0 then
        return nil, "no_path"
    end
    if #path > PATH_WAYPOINT_LIMIT then
        return nil, "path_too_long"
    end
    local waypoints = {}
    for _, point in ipairs(path) do
        waypoints[#waypoints + 1] = position_array(point)
    end
    return waypoints, nil
end

local function standing_candidates_near(target, player_pos, maximum_distance)
    local candidates = {}
    local target_pos = vector.round(target)
    local horizontal = math.max(2, math.ceil(maximum_distance))
    for y = -2, 2 do
        for x = -horizontal, horizontal do
            for z = -horizontal, horizontal do
                local distance = math.sqrt(x * x + y * y + z * z)
                if distance >= 1 and distance <= maximum_distance then
                    local stand = {
                        x = target_pos.x + x,
                        y = target_pos.y + y,
                        z = target_pos.z + z,
                    }
                    if safe_standing_position(stand) then
                        candidates[#candidates + 1] = {
                            pos = stand,
                            target_distance = distance,
                            player_distance = vector.distance(player_pos, stand),
                        }
                    end
                end
            end
        end
    end
    table.sort(candidates, function(a, b)
        if math.abs(a.target_distance - b.target_distance) > 0.01 then
            return a.target_distance < b.target_distance
        end
        return a.player_distance < b.player_distance
    end)
    return candidates
end

local function plan_path_near(player, target, maximum_distance, attempt_budget, search_distance)
    local player_pos = player:get_pos()
    local candidates = standing_candidates_near(target, player_pos, maximum_distance)
    local attempts = 0
    search_distance = clamp_integer(search_distance, 6, 2, PATH_RADIUS_MAX)
    for _, candidate in ipairs(candidates) do
        if attempts >= attempt_budget then
            break
        end
        attempts = attempts + 1
        local path = path_to(player, candidate.pos, search_distance)
        if path then
            return path, candidate.pos, attempts
        end
    end
    return nil, nil, attempts
end

local function parse_node_selector(selector)
    if type(selector) ~= "string" or selector == "" then
        return nil
    end
    if minetest.registered_nodes[selector] then
        return selector
    end
    if string.match(selector, "^group:[%w_]+$") then
        return selector
    end
    return nil
end

local function plan_node_path(player, selector, radius, require_unprotected)
    local center = vector.round(player:get_pos())
    local attempts = 0
    local considered = 0
    local target_found = false
    local seen = {}
    local scan_radius = math.min(4, radius)
    while scan_radius <= radius
            and considered < PATH_NODE_CANDIDATE_LIMIT
            and attempts < PATH_ATTEMPT_LIMIT do
        local extent = { x = scan_radius, y = scan_radius, z = scan_radius }
        local found = minetest.find_nodes_in_area(
            vector.subtract(center, extent),
            vector.add(center, extent),
            { selector }
        )
        local targets = {}
        for _, target in ipairs(found) do
            local key = target.x .. ":" .. target.y .. ":" .. target.z
            if not seen[key] then
                seen[key] = true
                targets[#targets + 1] = target
            end
        end
        table.sort(targets, function(a, b)
            return vector.distance(center, a) < vector.distance(center, b)
        end)
        for _, target in ipairs(targets) do
            if considered >= PATH_NODE_CANDIDATE_LIMIT or attempts >= PATH_ATTEMPT_LIMIT then
                break
            end
            target_found = true
            local node = minetest.get_node_or_nil(target)
            local def = node and minetest.registered_nodes[node.name] or nil
            local protected = require_unprotected
                and minetest.is_protected(target, player:get_player_name())
            if node and node.name ~= "ignore" and def and not protected
                    and (not require_unprotected or def.diggable ~= false) then
                considered = considered + 1
                local path, stand, used = plan_path_near(
                    player,
                    target,
                    4.5,
                    math.min(12, PATH_ATTEMPT_LIMIT - attempts),
                    math.min(PATH_RADIUS_MAX, scan_radius + 6)
                )
                attempts = attempts + used
                if path then
                    return {
                        selector = selector,
                        node = node.name,
                        target = position_array(target),
                        stand = position_array(stand),
                        path = path,
                        waypoint_count = #path,
                        distance = math.floor(vector.distance(center, target) * 10 + 0.5) / 10,
                        scanned_radius = scan_radius,
                    }
                end
            end
        end
        if scan_radius == radius then
            break
        end
        scan_radius = math.min(radius, scan_radius + 4)
    end
    if not target_found then
        return nil, "target_not_found"
    end
    return nil, "no_reachable_target"
end

local function parse_pos_params(param)
    local words = split_words(param)
    local nums = {}
    for index = 1, math.min(#words, 3) do
        local value = words[index]
        local n = tonumber(value)
        if not n then
            return nil
        end
        nums[#nums + 1] = n
    end
    if #nums >= 3 then
        return { x = nums[1], y = nums[2], z = nums[3] }
    end
    return nil
end

local function front_pos(player)
    local pos = player:get_pos()
    local dir = player:get_look_dir()
    if not dir then
        return vector.round(pos)
    end
    local target = {
        x = pos.x + dir.x,
        y = pos.y + dir.y,
        z = pos.z + dir.z,
    }
    return vector.round(target)
end

local function find_inventory_item(inv, item_name)
    local list = inv:get_list("main") or {}
    for idx, stack in ipairs(list) do
        if stack and not stack:is_empty() and stack:get_name() == item_name then
            return idx, stack
        end
    end
    return nil, nil
end

local function wield_inventory_slot(player, inv, slot, stack)
    if type(player.get_wield_index) ~= "function"
            or type(player.get_wield_list) ~= "function"
            or type(player.set_wielded_item) ~= "function" then
        return false, "wield API unavailable"
    end
    local wield_list = player:get_wield_list()
    local wield_index = player:get_wield_index()
    if wield_list == "main" and wield_index == slot then
        return true
    end
    local previous = player:get_wielded_item()
    if not player:set_wielded_item(stack) then
        return false, "could not set wielded item"
    end
    if not inv:set_stack("main", slot, previous) then
        player:set_wielded_item(previous)
        return false, "could not update source inventory slot"
    end
    return true
end

local function adjacent_node_toward(target, observer)
    local delta = vector.subtract(observer, target)
    local abs_x = math.abs(delta.x)
    local abs_y = math.abs(delta.y)
    local abs_z = math.abs(delta.z)
    local offset = { x = 0, y = 0, z = 0 }

    if abs_x >= abs_y and abs_x >= abs_z and abs_x > 0 then
        offset.x = delta.x >= 0 and 1 or -1
    elseif abs_y >= abs_z and abs_y > 0 then
        offset.y = delta.y >= 0 and 1 or -1
    elseif abs_z > 0 then
        offset.z = delta.z >= 0 and 1 or -1
    else
        offset.y = 1
    end

    return vector.add(target, offset)
end

local function player_eye_position(player, pos)
    local eye_height = 1.625
    if type(player.get_properties) == "function" then
        local properties = player:get_properties()
        if properties and type(properties.eye_height) == "number" then
            eye_height = properties.eye_height
        end
    end
    return { x = pos.x, y = pos.y + eye_height, z = pos.z }
end

local function node_allows_dig(def, target, player)
    if not def or def.diggable == false then
        return false, "not_diggable"
    end
    if type(def.can_dig) ~= "function" then
        return true
    end

    local callback_ok, allowed = pcall(
        def.can_dig,
        { x = target.x, y = target.y, z = target.z },
        player
    )
    if not callback_ok then
        minetest.log("warning", "[llm_bot] node can_dig callback failed: " .. tostring(allowed))
        return false, "can_dig_failed"
    end
    if not allowed then
        return false, "cannot_dig"
    end
    return true
end

local function harvest_capability(node_name, tool_name, player)
    if type(mcl_autogroup) ~= "table"
            or type(mcl_autogroup.can_harvest) ~= "function" then
        -- Games without tiered harvesting yield their normal drops for any
        -- tool which minetest.get_dig_params reports as diggable.
        return true, false
    end

    local ok, harvestable = pcall(
        mcl_autogroup.can_harvest,
        node_name,
        tool_name,
        player
    )
    if not ok then
        minetest.log("warning", "[llm_bot] harvest check failed: " .. tostring(harvestable))
        return false, true
    end
    return harvestable == true, true
end

local function stack_has_own_toolcaps(stack)
    local def = stack:get_definition()
    if def and def.tool_capabilities then
        return true
    end
    local meta = stack:get_meta()
    return meta and type(meta.contains) == "function"
        and meta:contains("tool_capabilities") or false
end

local function dig_candidate(
        node_name, groups, stack, hand_stack, slot, effective_tool, player, is_hand)
    -- The server uses the player's custom hand capabilities when a selected
    -- item has no capabilities of its own. ItemStack:get_tool_capabilities()
    -- alone only falls back to the generic registered hand (""), so mirror
    -- the server's behavior explicitly for games with a custom hand item.
    local capabilities_stack = stack
    local using_hand_caps = is_hand or not stack_has_own_toolcaps(stack)
    if using_hand_caps then
        capabilities_stack = hand_stack
    end
    local toolcaps = capabilities_stack:get_tool_capabilities()
    local wear = is_hand and capabilities_stack:get_wear() or stack:get_wear()
    local ok, params = pcall(minetest.get_dig_params, groups, toolcaps, wear)
    if (not ok or type(params) ~= "table" or params.diggable ~= true)
            and not using_hand_caps then
        -- Luanti's client and server both retry with the hand when a selected
        -- tool cannot dig the node. This also permits safe hand-speed digging
        -- when every main inventory slot is occupied.
        toolcaps = hand_stack:get_tool_capabilities()
        ok, params = pcall(minetest.get_dig_params, groups, toolcaps)
        using_hand_caps = true
    end
    if not ok or type(params) ~= "table" or params.diggable ~= true
            or type(params.time) ~= "number" then
        return nil
    end

    local tool_name = effective_tool
    if not tool_name or tool_name == "" then
        tool_name = is_hand and "hand" or stack:get_name()
    end
    local harvest_name = is_hand and "" or stack:get_name()
    local harvestable, harvest_supported = harvest_capability(
        node_name,
        harvest_name,
        player
    )
    return {
        slot = slot,
        stack = stack,
        tool = tool_name,
        dig_time = math.max(0, params.time),
        harvestable = harvestable,
        harvest_supported = harvest_supported,
        is_hand = is_hand,
    }
end

local function candidate_is_better(candidate, current, wield_index)
    if not current then
        return true
    end
    if candidate.harvest_supported and candidate.harvestable ~= current.harvestable then
        return candidate.harvestable
    end
    if candidate.dig_time ~= current.dig_time then
        return candidate.dig_time < current.dig_time
    end

    local candidate_is_wielded = candidate.slot == wield_index
    local current_is_wielded = current.slot == wield_index
    if candidate_is_wielded ~= current_is_wielded then
        return candidate_is_wielded
    end
    if candidate.is_hand ~= current.is_hand then
        return not candidate.is_hand
    end
    return candidate.slot < current.slot
end

local function fastest_dig_candidate(player, inv, node_name, groups)
    if type(player.get_wield_index) ~= "function" then
        return nil, "wield_api_unavailable"
    end
    local wield_index = player:get_wield_index()
    local list = inv:get_list("main") or {}
    local hand_stack = inv:get_stack("hand", 1)
    if not hand_stack or hand_stack:is_empty() then
        hand_stack = ItemStack("")
    end
    local best = nil
    local empty_slot = nil

    for slot, stack in ipairs(list) do
        if stack and stack:is_empty() then
            empty_slot = empty_slot or slot
        elseif stack then
            local candidate = dig_candidate(
                node_name,
                groups,
                stack,
                hand_stack,
                slot,
                stack:get_name(),
                player,
                false
            )
            if candidate and candidate_is_better(candidate, best, wield_index) then
                best = candidate
            end
        end
    end

    -- An empty main-list slot makes the game's effective hand item active.
    -- Reusing an actual empty slot lets wield_inventory_slot preserve whatever
    -- was selected before without dropping or overwriting it.
    local hand_slot = empty_slot
    if list[wield_index] and list[wield_index]:is_empty() then
        hand_slot = wield_index
    end
    if hand_slot then
        local effective_tool = hand_stack:get_name()
        local candidate = dig_candidate(
            node_name,
            groups,
            hand_stack,
            hand_stack,
            hand_slot,
            effective_tool,
            player,
            true
        )
        if candidate then
            candidate.stack = list[hand_slot] or ItemStack("")
            if candidate_is_better(candidate, best, wield_index) then
                best = candidate
            end
        end
    end

    if not best then
        return nil, "no_diggable_tool"
    end
    if best.harvest_supported and not best.harvestable then
        return nil, "no_harvest_tool"
    end
    return best
end

local CHEST_INTERACTION_RANGE = 6
local CHEST_OBSERVATION_LIMIT = 8
local CHEST_CONTENT_LIMIT = 54
local FURNACE_INTERACTION_RANGE = 6
local FURNACE_OBSERVATION_LIMIT = 8
local FURNACE_OPTION_LIMIT = 16
local INVENTORY_RECEIPT_TTL_SECONDS = 3
local CRAFT_INTERACTION_RANGE = 6
local CRAFT_MAX_BATCHES = 8
local CRAFT_MAX_MOVES = 36
local CRAFT_MAX_OUTPUT_COUNT = 64
local CRAFTABLE_OBSERVATION_LIMIT = 32
local CRAFT_RECEIPT_TTL_SECONDS = 12
local inventory_action_receipts = {}

local FURNACE_NODE_SPECS = {
    ["mcl_furnaces:furnace"] = {
        kind = "furnace", active = false, speed = 1,
    },
    ["mcl_furnaces:furnace_active"] = {
        kind = "furnace", active = true, speed = 1,
    },
    ["mcl_blast_furnace:blast_furnace"] = {
        kind = "blast_furnace", active = false, speed = 2,
        input_group = "blast_furnace_smeltable",
    },
    ["mcl_blast_furnace:blast_furnace_active"] = {
        kind = "blast_furnace", active = true, speed = 2,
        input_group = "blast_furnace_smeltable",
    },
    ["mcl_smoker:smoker"] = {
        kind = "smoker", active = false, speed = 2,
        input_group = "smoker_cookable",
    },
    ["mcl_smoker:smoker_active"] = {
        kind = "smoker", active = true, speed = 2,
        input_group = "smoker_cookable",
    },
}

local function furnace_node_spec(node_name)
    return FURNACE_NODE_SPECS[node_name]
end

local function chest_node_kind(node_name)
    if node_name == "mcl_barrels:barrel_closed"
            or node_name == "mcl_barrels:barrel_open" then
        return "barrel", "small", node_name
    end
    if type(node_name) ~= "string"
            or node_name:sub(1, 11) ~= "mcl_chests:"
            or node_name:find("ender_chest", 1, true) then
        return nil
    end
    if minetest.get_item_group(node_name, "shulker_box") > 0
            and node_name:sub(-6) == "_small" then
        return "shulker_box", "small", node_name:sub(1, -7)
    end

    local side = nil
    local basename = nil
    for _, suffix in ipairs({ "small", "left", "right" }) do
        local marker = "_" .. suffix
        if node_name:sub(-#marker) == marker then
            side = suffix
            basename = node_name:sub(1, #node_name - #marker)
            break
        end
    end
    if not side or not (
            basename == "mcl_chests:chest"
            or basename == "mcl_chests:trapped_chest"
            or basename == "mcl_chests:trapped_chest_on") then
        return nil
    end
    local kind = basename:find("trapped_chest", 1, true)
        and "trapped_chest" or "chest"
    return kind, side, basename
end

local function double_chest_neighbor(pos, param2, side)
    if type(mcl_util) == "table"
            and type(mcl_util.get_double_container_neighbor_pos) == "function" then
        return mcl_util.get_double_container_neighbor_pos(pos, param2, side)
    end
    local sign = side == "right" and 1 or -1
    if param2 == 0 then
        return vector.offset(pos, -sign, 0, 0)
    elseif param2 == 1 then
        return vector.offset(pos, 0, 0, sign)
    elseif param2 == 2 then
        return vector.offset(pos, sign, 0, 0)
    elseif param2 == 3 then
        return vector.offset(pos, 0, 0, -sign)
    end
    return nil
end

local function chest_top_is_clear(pos)
    local above = minetest.get_node_or_nil(vector.offset(pos, 0, 1, 0))
    if not above then
        return false
    end
    local def = minetest.registered_nodes[above.name]
    return def ~= nil and ((def.groups and def.groups.opaque) or 0) ~= 1
end

local function same_node_position(a, b)
    return a and b
        and a.x == b.x
        and a.y == b.y
        and a.z == b.z
end

local function positions_are_visible(player, positions)
    local player_pos = player:get_pos()
    if not player_pos then
        return false
    end
    local eye_pos = player_eye_position(player, player_pos)

    for _, pos in ipairs(positions) do
        local target = { x = pos.x, y = pos.y, z = pos.z }
        if type(minetest.raycast) == "function" then
            local ok, visible = pcall(function()
                for pointed in minetest.raycast(eye_pos, target, false, false) do
                    if pointed.type == "node" then
                        return same_node_position(pointed.under, pos)
                    end
                end
                return false
            end)
            if ok and visible then
                return true
            end
        elseif type(minetest.line_of_sight) == "function" then
            local ok, clear, blocker = pcall(minetest.line_of_sight, eye_pos, target)
            if ok and (clear or same_node_position(blocker, pos)) then
                return true
            end
        end
    end
    return false
end

local function chest_inventory_descriptor(pos)
    local inv = minetest.get_meta(pos):get_inventory()
    if not inv or inv:get_size("main") <= 0 then
        return nil
    end
    return {
        pos = { x = pos.x, y = pos.y, z = pos.z },
        inv = inv,
        list = "main",
    }
end

local function resolve_chest(player, requested_pos)
    local node = minetest.get_node_or_nil(requested_pos)
    if not node then
        return nil, "unloaded"
    end
    local kind, side, basename = chest_node_kind(node.name)
    if not kind then
        return nil, "not_supported_chest"
    end

    local positions = {}
    local canonical_pos = { x = requested_pos.x, y = requested_pos.y, z = requested_pos.z }
    if side == "small" then
        positions[1] = canonical_pos
    else
        local neighbor = double_chest_neighbor(requested_pos, node.param2, side)
        if not neighbor then
            return nil, "invalid_double_chest"
        end
        local neighbor_node = minetest.get_node_or_nil(neighbor)
        local expected_side = side == "left" and "right" or "left"
        if not neighbor_node
                or neighbor_node.name ~= basename .. "_" .. expected_side
                or neighbor_node.param2 ~= node.param2 then
            return nil, "incomplete_double_chest"
        end
        if side == "left" then
            positions = { canonical_pos, neighbor }
        else
            canonical_pos = { x = neighbor.x, y = neighbor.y, z = neighbor.z }
            positions = { canonical_pos, requested_pos }
            node = neighbor_node
        end
        kind = kind == "trapped_chest" and "double_trapped_chest" or "double_chest"
    end

    local inventories = {}
    for _, pos in ipairs(positions) do
        local descriptor = chest_inventory_descriptor(pos)
        if not descriptor then
            return nil, "missing_chest_inventory"
        end
        inventories[#inventories + 1] = descriptor
    end

    local player_pos = player:get_pos()
    local distance = math.huge
    if player_pos then
        for _, descriptor in ipairs(inventories) do
            descriptor.distance = vector.distance(player_pos, descriptor.pos)
            descriptor.reachable = descriptor.distance <= CHEST_INTERACTION_RANGE
            distance = math.min(distance, descriptor.distance)
        end
        table.sort(inventories, function(a, b)
            if a.reachable ~= b.reachable then
                return a.reachable
            end
            if a.distance ~= b.distance then
                return a.distance < b.distance
            end
            if a.pos.x ~= b.pos.x then
                return a.pos.x < b.pos.x
            end
            if a.pos.y ~= b.pos.y then
                return a.pos.y < b.pos.y
            end
            return a.pos.z < b.pos.z
        end)
    end
    local accessible = true
    local access_status = "accessible"
    if distance > CHEST_INTERACTION_RANGE then
        accessible = false
        access_status = "out_of_range"
    elseif not positions_are_visible(player, positions) then
        accessible = false
        access_status = "blocked_path"
    else
        for _, pos in ipairs(positions) do
            if minetest.is_protected(pos, player:get_player_name()) then
                accessible = false
                access_status = "protected"
                break
            end
        end
    end
    if accessible and kind ~= "shulker_box" and kind ~= "barrel" then
        for _, pos in ipairs(positions) do
            if not chest_top_is_clear(pos) then
                accessible = false
                access_status = "blocked"
                break
            end
        end
    end

    return {
        requested_pos = { x = requested_pos.x, y = requested_pos.y, z = requested_pos.z },
        pos = canonical_pos,
        node = node.name,
        kind = kind,
        inventories = inventories,
        distance = distance,
        accessible = accessible,
        access_status = access_status,
    }
end

local function inventory_item_count(inventories, item_name)
    local total = 0
    for _, descriptor in ipairs(inventories) do
        for _, stack in ipairs(descriptor.inv:get_list(descriptor.list) or {}) do
            if stack and not stack:is_empty() and stack:get_name() == item_name then
                total = total + stack:get_count()
            end
        end
    end
    return total
end

local function inventory_room_for_stack(inv, list_name, stack, limit)
    local remaining = limit
    local capacity = 0
    local stack_max = math.max(1, stack:get_stack_max())
    for index = 1, inv:get_size(list_name) do
        if remaining <= 0 then
            break
        end
        local offered_count = math.min(remaining, stack_max)
        local offered = ItemStack(stack)
        offered:set_count(offered_count)
        local destination = ItemStack(inv:get_stack(list_name, index))
        local leftover = destination:add_item(offered)
        local accepted = offered_count - leftover:get_count()
        capacity = capacity + accepted
        remaining = remaining - accepted
    end
    return capacity
end

local BASIC_CRAFT_ITEMS = {
    ["mcl_core:stick"] = true,
    ["mcl_torches:torch"] = true,
    ["mcl_crafting_table:crafting_table"] = true,
    ["mcl_furnaces:furnace"] = true,
    ["mcl_tools:pick_wood"] = true,
    ["mcl_tools:pick_stone"] = true,
    ["mcl_tools:axe_wood"] = true,
    ["mcl_tools:axe_stone"] = true,
    ["mcl_tools:shovel_wood"] = true,
    ["mcl_tools:shovel_stone"] = true,
    ["mcl_tools:sword_wood"] = true,
    ["mcl_tools:sword_stone"] = true,
    ["mcl_farming:hoe_wood"] = true,
    ["mcl_farming:hoe_stone"] = true,
}

local function basic_craft_item_allowed(item_name)
    return minetest.registered_items[item_name] ~= nil
        and (BASIC_CRAFT_ITEMS[item_name]
            or minetest.get_item_group(item_name, "wood") > 0)
end

local basic_craft_outputs_cache = nil

local function basic_craft_outputs()
    if basic_craft_outputs_cache then
        return basic_craft_outputs_cache
    end
    local outputs = {}
    for item_name in pairs(minetest.registered_items) do
        if basic_craft_item_allowed(item_name) then
            outputs[#outputs + 1] = item_name
        end
    end
    table.sort(outputs)
    basic_craft_outputs_cache = outputs
    return outputs
end

local function crafting_lists_empty(inv)
    if not inv then
        return false
    end
    return inv:is_empty("craft") and inv:is_empty("craftresult")
end

local function reset_crafting_grid(player)
    local inv = player and player:get_inventory() or nil
    if not inv or not crafting_lists_empty(inv) then
        return false
    end
    inv:set_width("craft", 2)
    inv:set_size("craft", 4)
    return inv:get_size("craft") == 4
end

local function nearby_crafting_table(player)
    local player_pos = player:get_pos()
    if not player_pos then
        return nil
    end
    local center = vector.round(player_pos)
    local radius = math.ceil(CRAFT_INTERACTION_RANGE)
    local candidates = {}
    local minimum = vector.offset(center, -radius, -radius, -radius)
    local maximum = vector.offset(center, radius, radius, radius)
    for _, pos in ipairs(minetest.find_nodes_in_area(
            minimum, maximum, { "mcl_crafting_table:crafting_table" })) do
        local distance = vector.distance(player_pos, pos)
        if distance <= CRAFT_INTERACTION_RANGE then
            candidates[#candidates + 1] = {
                pos = { x = pos.x, y = pos.y, z = pos.z },
                distance = distance,
            }
        end
    end
    table.sort(candidates, function(a, b)
        return a.distance < b.distance
    end)
    for _, candidate in ipairs(candidates) do
        if not minetest.is_protected(candidate.pos, player:get_player_name())
                and positions_are_visible(player, { candidate.pos }) then
            return candidate
        end
    end
    return nil
end

local function craft_recipe_has_replacements(recipe)
    return type(recipe.replacements) == "table" and next(recipe.replacements) ~= nil
end

local function craft_recipe_entries(recipe)
    local method = recipe.method or recipe.type or "normal"
    if method ~= "normal" or craft_recipe_has_replacements(recipe) then
        return nil, "unsupported_recipe"
    end
    local items = type(recipe.items) == "table" and recipe.items or {}
    local source_indices = {}
    local maximum_index = 0
    for index, ingredient in pairs(items) do
        if type(index) == "number" and index >= 1 and index == math.floor(index)
                and type(ingredient) == "string" and ingredient ~= "" then
            source_indices[#source_indices + 1] = index
            maximum_index = math.max(maximum_index, index)
        end
    end
    table.sort(source_indices)
    if #source_indices == 0 or #source_indices > 9 then
        return nil, "unsupported_recipe"
    end

    local width = math.floor(tonumber(recipe.width) or 0)
    local table_required = false
    local grid_width = 2
    if width > 0 then
        local height = math.ceil(maximum_index / width)
        if width > 3 or height > 3 then
            return nil, "unsupported_recipe"
        end
        table_required = width > 2 or height > 2
    else
        table_required = #source_indices > 4
    end
    if table_required then
        grid_width = 3
    end

    local entries = {}
    for order, source_index in ipairs(source_indices) do
        local craft_slot
        if width > 0 then
            local row = math.floor((source_index - 1) / width)
            local column = (source_index - 1) % width
            craft_slot = row * grid_width + column
        else
            craft_slot = order - 1
        end
        entries[#entries + 1] = {
            ingredient = items[source_index],
            craft_slot = craft_slot,
            group = items[source_index]:sub(1, 6) == "group:",
        }
    end
    return {
        entries = entries,
        grid_width = grid_width,
        table_required = table_required,
    }
end

local function craft_ingredient_matches(item_name, ingredient)
    if ingredient:sub(1, 6) ~= "group:" then
        return item_name == ingredient
    end
    local found_group = false
    for group_name in ingredient:sub(7):gmatch("[^,]+") do
        found_group = true
        if minetest.get_item_group(item_name, group_name) <= 0 then
            return false
        end
    end
    return found_group
end

local function craft_stack_variant_key(stack)
    local unit = ItemStack(stack)
    unit:set_count(1)
    return unit:to_string()
end

local function craft_inventory_variants(inv)
    local variants = {}
    local by_key = {}
    for player_slot, stack in ipairs(inv:get_list("main") or {}) do
        if stack and not stack:is_empty() then
            local key = craft_stack_variant_key(stack)
            local variant = by_key[key]
            if not variant then
                local unit = ItemStack(stack)
                unit:set_count(1)
                variant = {
                    key = key,
                    name = stack:get_name(),
                    stack = unit,
                    slots = {},
                }
                by_key[key] = variant
                variants[#variants + 1] = variant
            end
            variant.slots[#variant.slots + 1] = {
                player_slot = player_slot - 1,
                count = stack:get_count(),
            }
        end
    end
    table.sort(variants, function(a, b)
        if a.name ~= b.name then
            return a.name < b.name
        end
        return a.key < b.key
    end)
    return variants
end

local function craft_variant_available(variant, reserved)
    local available = 0
    for _, source in ipairs(variant.slots) do
        available = available + math.max(
            0, source.count - (reserved[source.player_slot] or 0)
        )
    end
    return available
end

local function allocate_craft_ingredients(inv, entries, batches, grid_width)
    local variants = craft_inventory_variants(inv)
    local reserved = {}
    local moves = {}
    local grid = {}
    for index = 1, grid_width * grid_width do
        grid[index] = ItemStack("")
    end

    local allocation_order = {}
    for index, entry in ipairs(entries) do
        allocation_order[index] = entry
    end
    table.sort(allocation_order, function(a, b)
        if a.group ~= b.group then
            return not a.group
        end
        return a.craft_slot < b.craft_slot
    end)

    local ingredient_counts = {}
    for _, entry in ipairs(allocation_order) do
        local candidates = {}
        for _, variant in ipairs(variants) do
            if craft_ingredient_matches(variant.name, entry.ingredient) then
                local available = craft_variant_available(variant, reserved)
                if available >= batches then
                    candidates[#candidates + 1] = {
                        variant = variant,
                        available = available,
                    }
                end
            end
        end
        table.sort(candidates, function(a, b)
            if a.available ~= b.available then
                return a.available > b.available
            end
            if a.variant.name ~= b.variant.name then
                return a.variant.name < b.variant.name
            end
            return a.variant.key < b.variant.key
        end)
        local selected = candidates[1] and candidates[1].variant or nil
        if not selected then
            return nil, "insufficient_ingredients"
        end

        local remaining = batches
        for _, source in ipairs(selected.slots) do
            if remaining <= 0 then
                break
            end
            local already_reserved = reserved[source.player_slot] or 0
            local available = math.max(0, source.count - already_reserved)
            local moved = math.min(remaining, available)
            if moved > 0 then
                reserved[source.player_slot] = already_reserved + moved
                moves[#moves + 1] = {
                    player_slot = source.player_slot,
                    craft_slot = entry.craft_slot,
                    count = moved,
                }
                remaining = remaining - moved
            end
        end
        if remaining > 0 then
            return nil, "insufficient_ingredients"
        end

        local grid_stack = ItemStack(selected.stack)
        grid_stack:set_count(batches)
        grid[entry.craft_slot + 1] = grid_stack
        ingredient_counts[selected.name] =
            (ingredient_counts[selected.name] or 0) + batches
    end
    if #moves > CRAFT_MAX_MOVES then
        return nil, "too_many_inventory_moves"
    end
    table.sort(moves, function(a, b)
        if a.craft_slot ~= b.craft_slot then
            return a.craft_slot < b.craft_slot
        end
        return a.player_slot < b.player_slot
    end)
    local ingredients = {}
    for item_name, count in pairs(ingredient_counts) do
        ingredients[#ingredients + 1] = { item = item_name, count = count }
    end
    table.sort(ingredients, function(a, b)
        return a.item < b.item
    end)
    return {
        moves = moves,
        grid = grid,
        ingredients = ingredients,
    }
end

local function simulated_output_fits(inv, moves, output_stack)
    local simulated = {}
    for index, stack in ipairs(inv:get_list("main") or {}) do
        simulated[index] = ItemStack(stack)
    end
    for _, move in ipairs(moves) do
        local source = simulated[move.player_slot + 1]
        if not source or source:is_empty() then
            return false
        end
        local taken = source:take_item(move.count)
        if taken:get_count() ~= move.count then
            return false
        end
    end
    local remaining = ItemStack(output_stack)
    for _, destination in ipairs(simulated) do
        if remaining:is_empty() then
            break
        end
        remaining = destination:add_item(remaining)
    end
    return remaining:is_empty()
end

local function plan_craft_recipe(player, item_name, recipe, batches, table_info)
    local recipe_shape, shape_status = craft_recipe_entries(recipe)
    if not recipe_shape then
        return nil, shape_status
    end
    if recipe_shape.table_required and not table_info then
        return nil, "crafting_table_required"
    end
    local inv = player:get_inventory()
    local allocation, allocation_status = allocate_craft_ingredients(
        inv, recipe_shape.entries, batches, recipe_shape.grid_width
    )
    if not allocation then
        return nil, allocation_status
    end

    local output = minetest.get_craft_result({
        method = "normal",
        width = recipe_shape.grid_width,
        items = allocation.grid,
    })
    local output_stack = output and ItemStack(output.item) or ItemStack("")
    if output_stack:is_empty() or output_stack:get_name() ~= item_name then
        return nil, "recipe_mismatch"
    end
    if type(output.replacements) == "table" and next(output.replacements) ~= nil then
        return nil, "recipe_replacements_unsupported"
    end
    local output_per_batch = output_stack:get_count()
    local produced = output_per_batch * batches
    if output_per_batch < 1 or produced > output_stack:get_stack_max() then
        return nil, "output_stack_limit"
    end
    local complete_output = ItemStack(output_stack)
    complete_output:set_count(produced)
    if not simulated_output_fits(inv, allocation.moves, complete_output) then
        return nil, "inventory_full"
    end
    return {
        item = item_name,
        batches = batches,
        produced = produced,
        output_per_batch = output_per_batch,
        grid_width = recipe_shape.grid_width,
        grid_size = recipe_shape.grid_width * recipe_shape.grid_width,
        table_required = recipe_shape.table_required,
        table_pos = table_info and position_array(table_info.pos) or nil,
        moves = allocation.moves,
        ingredients = allocation.ingredients,
    }
end

local CRAFT_ERROR_PRIORITY = {
    crafting_table_required = 1,
    insufficient_ingredients = 2,
    inventory_full = 3,
    output_stack_limit = 4,
    too_many_inventory_moves = 5,
    recipe_replacements_unsupported = 6,
    recipe_mismatch = 7,
    unsupported_recipe = 8,
}

local function preferred_craft_error(current, candidate)
    if not current then
        return candidate
    end
    local current_priority = CRAFT_ERROR_PRIORITY[current] or 100
    local candidate_priority = CRAFT_ERROR_PRIORITY[candidate] or 100
    return candidate_priority < current_priority and candidate or current
end

local function find_craft_plan(player, item_name, minimum_output, maximize, table_info)
    if not basic_craft_item_allowed(item_name) then
        return nil, "item_not_allowed"
    end
    local recipes = minetest.get_all_craft_recipes(item_name)
    if type(recipes) ~= "table" or #recipes == 0 then
        return nil, "no_recipe"
    end
    local best = nil
    local best_error = nil
    for _, recipe in ipairs(recipes) do
        local registered_output = ItemStack(recipe.output or "")
        local output_per_batch = registered_output:get_count()
        if not registered_output:is_empty()
                and registered_output:get_name() == item_name
                and output_per_batch > 0 then
            local maximum_batches = math.min(
                CRAFT_MAX_BATCHES,
                math.floor(registered_output:get_stack_max() / output_per_batch),
                math.floor(CRAFT_MAX_OUTPUT_COUNT / output_per_batch)
            )
            local first_batch
            local last_batch
            local step
            if maximize then
                first_batch = maximum_batches
                last_batch = 1
                step = -1
            else
                first_batch = math.ceil(minimum_output / output_per_batch)
                last_batch = first_batch
                step = 1
            end
            if first_batch >= 1 and first_batch <= maximum_batches then
                for batches = first_batch, last_batch, step do
                    local plan, status = plan_craft_recipe(
                        player, item_name, recipe, batches, table_info
                    )
                    if plan then
                        if not best or plan.produced > best.produced
                                or (plan.produced == best.produced
                                    and best.table_required and not plan.table_required) then
                            best = plan
                        end
                        break
                    end
                    best_error = preferred_craft_error(best_error, status)
                end
            elseif not maximize then
                best_error = preferred_craft_error(best_error, "batch_limit")
            end
        end
    end
    if best then
        return best
    end
    return nil, best_error or "no_supported_recipe"
end

local function activate_craft_grid(player, plan)
    local inv = player:get_inventory()
    if not inv or not crafting_lists_empty(inv) then
        return false, "craft_grid_busy"
    end
    if not plan.table_required then
        if not reset_crafting_grid(player) then
            return false, "craft_grid_reset_failed"
        end
        if inv:get_width("craft") < 2 or inv:get_size("craft") < 4 then
            return false, "craft_grid_unavailable"
        end
        return true
    end

    local raw_pos = plan.table_pos
    local table_pos = raw_pos and {
        x = raw_pos[1], y = raw_pos[2], z = raw_pos[3],
    } or nil
    local node = table_pos and minetest.get_node_or_nil(table_pos) or nil
    local player_pos = player:get_pos()
    if not node or node.name ~= "mcl_crafting_table:crafting_table" then
        return false, "crafting_table_changed"
    end
    if not player_pos
            or vector.distance(player_pos, table_pos) > CRAFT_INTERACTION_RANGE then
        return false, "crafting_table_out_of_range"
    end
    if minetest.is_protected(table_pos, player:get_player_name()) then
        return false, "crafting_table_protected"
    end
    if not positions_are_visible(player, { table_pos }) then
        return false, "crafting_table_blocked"
    end
    if type(mcl_crafting_table) ~= "table"
            or type(mcl_crafting_table.show_crafting_form) ~= "function" then
        return false, "crafting_table_api_unavailable"
    end
    local ok = pcall(mcl_crafting_table.show_crafting_form, player)
    if not ok or inv:get_width("craft") < 3 or inv:get_size("craft") < 9 then
        reset_crafting_grid(player)
        return false, "crafting_table_open_failed"
    end
    return true
end

local function craftable_observation(player)
    local craftable = {}
    local inv = player:get_inventory()
    if not inv or not crafting_lists_empty(inv) then
        return { craftable = craftable }
    end
    local table_info = nearby_crafting_table(player)
    for _, item_name in ipairs(basic_craft_outputs()) do
        local plan = find_craft_plan(player, item_name, 1, true, table_info)
        if plan then
            local ingredients = {}
            for _, ingredient in ipairs(plan.ingredients) do
                ingredients[#ingredients + 1] = {
                    item = ingredient.item,
                    count = math.floor(ingredient.count / plan.batches),
                }
            end
            craftable[#craftable + 1] = {
                item = plan.item,
                output_per_batch = plan.output_per_batch,
                max_batches = plan.batches,
                table_required = plan.table_required,
                ingredients = ingredients,
            }
            if #craftable >= CRAFTABLE_OBSERVATION_LIMIT then
                break
            end
        end
    end
    return { craftable = craftable }
end

local function chest_contents(chest)
    local counts = {}
    local used_slots = 0
    local slot_count = 0
    for _, descriptor in ipairs(chest.inventories) do
        slot_count = slot_count + descriptor.inv:get_size(descriptor.list)
        for _, stack in ipairs(descriptor.inv:get_list(descriptor.list) or {}) do
            if stack and not stack:is_empty() then
                used_slots = used_slots + 1
                counts[stack:get_name()] = (counts[stack:get_name()] or 0) + stack:get_count()
            end
        end
    end
    local names = {}
    for item_name in pairs(counts) do
        names[#names + 1] = item_name
    end
    table.sort(names)
    local contents = {}
    for index = 1, math.min(#names, CHEST_CONTENT_LIMIT) do
        local item_name = names[index]
        contents[#contents + 1] = { name = item_name, count = counts[item_name] }
    end
    return contents, #names > CHEST_CONTENT_LIMIT, used_slots, slot_count
end

local function chest_observation(chest)
    local result = {
        name = chest.node,
        kind = chest.kind,
        pos = position_array(chest.pos),
        distance = math.floor(chest.distance * 10 + 0.5) / 10,
        accessible = chest.accessible,
        status = chest.access_status,
        contents = {},
        contents_truncated = false,
    }
    if chest.accessible then
        result.contents, result.contents_truncated, result.used_slots, result.slots =
            chest_contents(chest)
    end
    return result
end

local function parse_integer_position(args, offset)
    local pos = {
        x = tonumber(args[offset]),
        y = tonumber(args[offset + 1]),
        z = tonumber(args[offset + 2]),
    }
    if not pos.x or not pos.y or not pos.z
            or pos.x ~= math.floor(pos.x)
            or pos.y ~= math.floor(pos.y)
            or pos.z ~= math.floor(pos.z) then
        return nil
    end
    return pos
end

local function prepare_chest_transfer(player, operation, requested_pos, item_name, requested_count)
    if operation ~= "deposit" and operation ~= "withdraw" then
        return nil, "invalid_operation"
    end
    if type(item_name) ~= "string" or item_name == ""
            or not minetest.registered_items[item_name] then
        return nil, "unknown_item"
    end
    if not requested_count or requested_count ~= math.floor(requested_count)
            or requested_count < 1 or requested_count > 99 then
        return nil, "invalid_count"
    end
    local chest, status = resolve_chest(player, requested_pos)
    if not chest then
        return nil, status
    end
    if not chest.accessible then
        return nil, chest.access_status
    end

    local player_inv = player:get_inventory()
    if not player_inv then
        return nil, "no_inventory"
    end
    local chest_count = inventory_item_count(chest.inventories, item_name)
    if operation == "deposit" then
        if chest.kind == "shulker_box"
                and minetest.get_item_group(item_name, "shulker_box") > 0 then
            return nil, "shulker_nesting_forbidden"
        end
        for player_slot, stack in ipairs(player_inv:get_list("main") or {}) do
            if stack and not stack:is_empty() and stack:get_name() == item_name then
                local wanted = math.min(requested_count, stack:get_count())
                for _, descriptor in ipairs(chest.inventories) do
                    local capacity = inventory_room_for_stack(
                        descriptor.inv, descriptor.list, stack, wanted
                    )
                    if capacity > 0 then
                        return {
                            operation = operation,
                            requested_target = position_array(requested_pos),
                            target = position_array(chest.pos),
                            node = chest.node,
                            item = item_name,
                            chest_count = chest_count,
                            action_count = math.min(wanted, capacity),
                            player_slot = player_slot - 1,
                            container_pos = position_array(descriptor.pos),
                        }
                    end
                end
            end
        end
        if find_inventory_item(player_inv, item_name) then
            return nil, "chest_full"
        end
        return nil, "item_not_found"
    end

    local saw_item = false
    for _, descriptor in ipairs(chest.inventories) do
        for chest_slot, stack in ipairs(descriptor.inv:get_list(descriptor.list) or {}) do
            if stack and not stack:is_empty() and stack:get_name() == item_name then
                saw_item = true
                local wanted = math.min(requested_count, stack:get_count())
                local capacity = inventory_room_for_stack(player_inv, "main", stack, wanted)
                if capacity > 0 then
                    return {
                        operation = operation,
                        requested_target = position_array(requested_pos),
                        target = position_array(chest.pos),
                        node = chest.node,
                        item = item_name,
                        chest_count = chest_count,
                        action_count = math.min(wanted, capacity),
                        chest_slot = chest_slot - 1,
                        container_pos = position_array(descriptor.pos),
                    }
                end
            end
        end
    end
    if saw_item then
        return nil, "inventory_full"
    end
    return nil, "item_not_found"
end

local function inventory_receipt_time()
    if type(minetest.get_us_time) == "function" then
        return minetest.get_us_time() / 1000000
    end
    return tonumber(minetest.get_gametime()) or 0
end

local function valid_action_nonce(nonce)
    return type(nonce) == "string"
        and #nonce >= 1
        and #nonce <= 64
        and nonce:match("^[%w_.%-]+$") ~= nil
end

local function active_inventory_receipt(name)
    local receipt = inventory_action_receipts[name]
    if not receipt then
        return nil, "not_found"
    end
    if inventory_receipt_time() > receipt.expires_at then
        inventory_action_receipts[name] = nil
        if receipt.adapter == "craft" then
            reset_crafting_grid(get_player(name))
        end
        return nil, "expired"
    end
    return receipt
end

local function active_adapter_receipt(name, adapter)
    local receipt, status = active_inventory_receipt(name)
    if not receipt then
        return nil, status
    end
    if receipt.adapter ~= adapter then
        return nil, "action_pending"
    end
    return receipt
end

local function expect_inventory_action(name, adapter, nonce, prepared)
    local existing = active_inventory_receipt(name)
    if existing then
        return nil, "action_pending"
    end

    local now = inventory_receipt_time()
    local receipt = {
        adapter = adapter,
        nonce = nonce,
        operation = prepared.operation,
        item = prepared.item,
        requested_target = prepared.requested_target,
        target = prepared.target,
        container_pos = prepared.container_pos,
        container_list = prepared.container_list or "main",
        container_slot = prepared.container_slot,
        node = prepared.node,
        kind = prepared.kind,
        action_count = prepared.action_count,
        player_slot = prepared.player_slot,
        player_action = prepared.player_action
            or (prepared.operation == "deposit" and "take" or "put"),
        moved = 0,
        completed = false,
        expires_at = now + INVENTORY_RECEIPT_TTL_SECONDS,
    }
    inventory_action_receipts[name] = receipt
    return receipt
end

local function parse_action_nonce(param)
    local args = split_words(param)
    local nonce = #args == 1 and args[1] or nil
    if not valid_action_nonce(nonce) then
        return nil
    end
    return nonce
end

local function inventory_transfer_receipt_details(receipt)
    local details = {
        nonce = receipt.nonce,
        operation = receipt.operation,
        item = receipt.item,
        requested_target = receipt.requested_target,
        target = receipt.target,
        container_pos = receipt.container_pos,
        container_list = receipt.container_list,
        container_slot = receipt.container_slot,
        action_count = receipt.action_count,
        moved = receipt.moved,
    }
    if receipt.adapter == "furnace" then
        details.node = receipt.node
        details.kind = receipt.kind
    end
    return details
end

local function report_inventory_transfer_receipt(name, param, adapter, tag)
    local nonce = parse_action_nonce(param)
    if not nonce then
        return command_result(name, tag, false, "invalid_parameters", "expected nonce")
    end

    local receipt, status = active_adapter_receipt(name, adapter)
    if not receipt then
        return command_result(name, tag, false, status, status, { nonce = nonce })
    end
    if receipt.nonce ~= nonce then
        return command_result(
            name, tag, false, "nonce_mismatch", "nonce mismatch", { nonce = nonce }
        )
    end

    local details = inventory_transfer_receipt_details(receipt)
    if not receipt.completed then
        return command_result(name, tag, false, "pending", "pending", details)
    end

    inventory_action_receipts[name] = nil
    return command_result(name, tag, true, "completed", "completed", details)
end

local function cancel_inventory_transfer(name, param, adapter, tag)
    local nonce = parse_action_nonce(param)
    if not nonce then
        return command_result(name, tag, false, "invalid_parameters", "expected nonce")
    end

    local receipt, status = active_adapter_receipt(name, adapter)
    if not receipt then
        return command_result(name, tag, false, status, status, { nonce = nonce })
    end
    if receipt.nonce ~= nonce then
        return command_result(
            name, tag, false, "nonce_mismatch", "nonce mismatch", { nonce = nonce }
        )
    end

    inventory_action_receipts[name] = nil
    return command_result(name, tag, true, "canceled", "canceled", { nonce = nonce })
end

local function inventory_main_item_count(inv, item_name)
    local total = 0
    for _, stack in ipairs(inv:get_list("main") or {}) do
        if stack and not stack:is_empty() and stack:get_name() == item_name then
            total = total + stack:get_count()
        end
    end
    return total
end

local function expect_craft_action(name, nonce, player, plan, requested)
    local existing = active_inventory_receipt(name)
    if existing then
        return nil, "action_pending"
    end
    local inv = player:get_inventory()
    local receipt = {
        adapter = "craft",
        nonce = nonce,
        item = plan.item,
        requested = requested,
        batches = plan.batches,
        produced = plan.produced,
        output_per_batch = plan.output_per_batch,
        grid_size = plan.grid_size,
        used_table = plan.table_required,
        table_pos = plan.table_pos,
        moves = plan.moves,
        ingredients = plan.ingredients,
        next_move = 1,
        ingredient_moved = 0,
        crafted_batches = 0,
        crafted_output = 0,
        delivered = 0,
        moved = 0,
        phase = #plan.moves > 0 and "ingredients" or "craft",
        initial_main_count = inventory_main_item_count(inv, plan.item),
        completed = false,
        expires_at = inventory_receipt_time() + CRAFT_RECEIPT_TTL_SECONDS,
    }
    inventory_action_receipts[name] = receipt
    return receipt
end

local function craft_receipt_details(receipt)
    return {
        nonce = receipt.nonce,
        item = receipt.item,
        requested = receipt.requested,
        batches = receipt.batches,
        produced = receipt.produced,
        output_per_batch = receipt.output_per_batch,
        grid_size = receipt.grid_size,
        used_table = receipt.used_table,
        table_pos = receipt.table_pos,
        ingredients = receipt.ingredients,
        ingredient_moved = receipt.ingredient_moved,
        crafted_batches = receipt.crafted_batches,
        crafted_output = receipt.crafted_output,
        delivered = receipt.delivered,
        moved = receipt.moved,
        phase = receipt.phase,
        error = receipt.error,
    }
end

local function handle_craft_inventory_action(receipt, action, info)
    if action ~= "move" then
        return
    end
    if receipt.phase == "ingredients" then
        local expected = receipt.moves[receipt.next_move]
        if not expected then
            receipt.phase = "craft"
            return
        end
        if info.from_list ~= "main" or info.to_list ~= "craft" then
            return
        end
        if info.from_index ~= expected.player_slot + 1
                or info.to_index ~= expected.craft_slot + 1
                or info.count ~= expected.count then
            receipt.error = "ingredient_move_mismatch"
            receipt.phase = "error"
            return
        end
        receipt.ingredient_moved = receipt.ingredient_moved + info.count
        receipt.next_move = receipt.next_move + 1
        if receipt.next_move > #receipt.moves then
            receipt.phase = "craft"
        end
        receipt.expires_at = inventory_receipt_time() + CRAFT_RECEIPT_TTL_SECONDS
        return
    end
    if receipt.phase == "output"
            and info.from_list == "craftresult" and info.to_list == "main" then
        if info.count < 1 or receipt.delivered + info.count > receipt.produced then
            receipt.error = "output_move_mismatch"
            receipt.phase = "error"
            return
        end
        receipt.delivered = receipt.delivered + info.count
        receipt.moved = receipt.delivered
        if receipt.delivered == receipt.produced then
            receipt.phase = "verify"
        end
        receipt.expires_at = inventory_receipt_time() + CRAFT_RECEIPT_TTL_SECONDS
    end
end

minetest.register_on_player_inventory_action(function(player, action, _, info)
    if not player or type(info) ~= "table" then
        return
    end
    local name = player:get_player_name()
    local receipt = active_inventory_receipt(name)
    if not receipt then
        return
    end

    if receipt.adapter == "craft" then
        handle_craft_inventory_action(receipt, action, info)
        return
    end

    if action ~= receipt.player_action or info.listname ~= "main" then
        return
    end
    if receipt.player_action == "take"
            and info.index ~= receipt.player_slot + 1 then
        return
    end

    local stack = info.stack
    if not stack or stack:is_empty() or stack:get_name() ~= receipt.item then
        return
    end
    local moved = stack:get_count()
    if moved < 1 or receipt.moved + moved > receipt.action_count then
        return
    end

    receipt.moved = receipt.moved + moved
    receipt.completed = true
    receipt.expires_at = inventory_receipt_time() + INVENTORY_RECEIPT_TTL_SECONDS
end)

minetest.register_on_craft(function(itemstack, player)
    if not player or not itemstack or itemstack:is_empty() then
        return
    end
    local receipt = active_inventory_receipt(player:get_player_name())
    if not receipt or receipt.adapter ~= "craft" or receipt.phase ~= "craft" then
        return
    end
    if itemstack:get_name() ~= receipt.item
            or itemstack:get_count() ~= receipt.output_per_batch then
        receipt.error = "crafted_output_mismatch"
        receipt.phase = "error"
        return
    end
    receipt.crafted_batches = receipt.crafted_batches + 1
    receipt.crafted_output = receipt.crafted_output + itemstack:get_count()
    if receipt.crafted_batches > receipt.batches
            or receipt.crafted_output > receipt.produced then
        receipt.error = "crafted_count_mismatch"
        receipt.phase = "error"
        return
    end
    if receipt.crafted_batches == receipt.batches then
        if receipt.crafted_output == receipt.produced then
            receipt.phase = "output"
        else
            receipt.error = "crafted_count_mismatch"
            receipt.phase = "error"
        end
    end
    receipt.expires_at = inventory_receipt_time() + CRAFT_RECEIPT_TTL_SECONDS
end)

minetest.register_on_leaveplayer(function(player)
    inventory_action_receipts[player:get_player_name()] = nil
end)

local function resolve_furnace(player, requested_pos)
    local node = minetest.get_node_or_nil(requested_pos)
    if not node then
        return nil, "unloaded"
    end
    local spec = furnace_node_spec(node.name)
    if not spec then
        return nil, "not_supported_furnace"
    end

    local inv = minetest.get_meta(requested_pos):get_inventory()
    if not inv
            or inv:get_size("src") ~= 1
            or inv:get_size("fuel") ~= 1
            or inv:get_size("dst") ~= 1 then
        return nil, "missing_furnace_inventory"
    end

    local target = { x = requested_pos.x, y = requested_pos.y, z = requested_pos.z }
    local player_pos = player:get_pos()
    local distance = player_pos and vector.distance(player_pos, target) or math.huge
    local accessible = true
    local access_status = "accessible"
    if distance > FURNACE_INTERACTION_RANGE then
        accessible = false
        access_status = "out_of_range"
    elseif not positions_are_visible(player, { target }) then
        accessible = false
        access_status = "blocked_path"
    elseif minetest.is_protected(target, player:get_player_name()) then
        accessible = false
        access_status = "protected"
    end

    return {
        pos = target,
        node = node.name,
        kind = spec.kind,
        active_node = spec.active,
        speed = spec.speed,
        input_group = spec.input_group,
        inv = inv,
        meta = minetest.get_meta(target),
        distance = distance,
        accessible = accessible,
        access_status = access_status,
    }
end

local function furnace_recipe_for_stack(furnace, stack)
    if not stack or stack:is_empty() then
        return nil, "empty_input"
    end
    if furnace.input_group
            and minetest.get_item_group(stack:get_name(), furnace.input_group) ~= 1 then
        if furnace.kind == "blast_furnace" then
            return nil, "not_blast_smeltable"
        end
        return nil, "not_smoker_cookable"
    end

    local offered = ItemStack(stack)
    local cooked = minetest.get_craft_result({
        method = "cooking",
        width = 1,
        items = { offered },
    })
    local cook_time = cooked and tonumber(cooked.time) or 0
    local output = cooked and ItemStack(cooked.item) or ItemStack("")
    if cook_time <= 0 or output:is_empty() then
        return nil, "not_cookable"
    end
    return { item = output, time = cook_time }
end

local function furnace_fuel_for_stack(stack)
    if not stack or stack:is_empty() then
        return nil, "empty_fuel"
    end
    local offered = ItemStack(stack)
    offered:set_count(1)
    local fuel, decremented = minetest.get_craft_result({
        method = "fuel",
        width = 1,
        items = { offered },
    })
    local burn_time = fuel and tonumber(fuel.time) or 0
    if burn_time <= 0 then
        return nil, "invalid_fuel"
    end
    local replacement = ItemStack("")
    if decremented and decremented.items and decremented.items[1] then
        replacement = ItemStack(decremented.items[1])
    end
    return { time = burn_time, replacement = replacement }
end

local function simple_stack_observation(stack)
    if not stack or stack:is_empty() then
        return nil
    end
    return { name = stack:get_name(), count = stack:get_count() }
end

local function furnace_inventory_options(player, furnace)
    local player_inv = player:get_inventory()
    local input_by_name = {}
    local fuel_by_name = {}
    if not player_inv then
        return {}, {}
    end

    for _, stack in ipairs(player_inv:get_list("main") or {}) do
        if stack and not stack:is_empty() then
            local item_name = stack:get_name()
            local recipe = furnace_recipe_for_stack(furnace, stack)
            if recipe then
                local option = input_by_name[item_name]
                if not option then
                    option = {
                        name = item_name,
                        count = 0,
                        output = simple_stack_observation(recipe.item),
                        cook_time = recipe.time,
                    }
                    input_by_name[item_name] = option
                end
                option.count = option.count + stack:get_count()
            end

            local fuel = furnace_fuel_for_stack(stack)
            if fuel then
                local option = fuel_by_name[item_name]
                if not option then
                    option = {
                        name = item_name,
                        count = 0,
                        burn_time = fuel.time,
                    }
                    if not fuel.replacement:is_empty() then
                        option.replacement = simple_stack_observation(fuel.replacement)
                    end
                    fuel_by_name[item_name] = option
                end
                option.count = option.count + stack:get_count()
            end
        end
    end

    local inputs = {}
    for _, option in pairs(input_by_name) do
        inputs[#inputs + 1] = option
    end
    table.sort(inputs, function(a, b)
        return a.name < b.name
    end)
    while #inputs > FURNACE_OPTION_LIMIT do
        table.remove(inputs)
    end

    local fuels = {}
    for _, option in pairs(fuel_by_name) do
        fuels[#fuels + 1] = option
    end
    table.sort(fuels, function(a, b)
        if a.burn_time ~= b.burn_time then
            return a.burn_time > b.burn_time
        end
        return a.name < b.name
    end)
    while #fuels > FURNACE_OPTION_LIMIT do
        table.remove(fuels)
    end
    return inputs, fuels
end

local function furnace_observation(player, furnace)
    local result = {
        name = furnace.node,
        kind = furnace.kind,
        pos = position_array(furnace.pos),
        distance = math.floor(furnace.distance * 10 + 0.5) / 10,
        accessible = furnace.accessible,
        status = furnace.access_status,
        active = furnace.active_node,
        speed = furnace.speed,
    }
    if not furnace.accessible then
        return result
    end

    local input = furnace.inv:get_stack("src", 1)
    local fuel_stack = furnace.inv:get_stack("fuel", 1)
    local output = furnace.inv:get_stack("dst", 1)
    result.input = simple_stack_observation(input)
    result.fuel = simple_stack_observation(fuel_stack)
    result.output = simple_stack_observation(output)

    local fuel_total = math.max(0, furnace.meta:get_float("fuel_totaltime"))
    local fuel_elapsed = math.max(0, furnace.meta:get_float("fuel_time"))
    local fuel_remaining = math.max(0, fuel_total - math.min(fuel_elapsed, fuel_total))
    result.fuel_total = fuel_total
    result.fuel_remaining = fuel_remaining
    result.active = furnace.active_node or fuel_remaining > 0

    local recipe, recipe_status = furnace_recipe_for_stack(furnace, input)
    result.cookable = recipe ~= nil
    result.output_blocked = false
    result.cook_progress = 0
    if recipe then
        result.recipe = {
            output = simple_stack_observation(recipe.item),
            cook_time = recipe.time,
        }
        result.output_blocked = not furnace.inv:room_for_item("dst", recipe.item)
        local source_time = math.max(0, furnace.meta:get_float("src_time"))
        result.cook_progress = math.max(0, math.min(1, source_time / recipe.time))
    end

    local queued_fuel = furnace_fuel_for_stack(fuel_stack)
    if result.output_blocked then
        result.activity = "output_blocked"
    elseif input:is_empty() and not output:is_empty() then
        result.activity = "output_ready"
    elseif input:is_empty() and fuel_remaining > 0 then
        result.activity = "burning_idle"
    elseif input:is_empty() then
        result.activity = "idle"
    elseif not recipe then
        result.activity = recipe_status or "invalid_input"
    elseif fuel_remaining > 0 then
        result.activity = "smelting"
    elseif queued_fuel then
        result.activity = "ready_to_start"
    else
        result.activity = "needs_fuel"
    end

    result.input_options, result.fuel_options = furnace_inventory_options(player, furnace)
    return result
end

local function prepared_furnace_action(furnace, operation, requested_pos, item_name, count)
    return {
        operation = operation,
        requested_target = position_array(requested_pos),
        target = position_array(furnace.pos),
        container_pos = position_array(furnace.pos),
        container_list = operation == "input" and "src"
            or operation == "fuel" and "fuel" or "dst",
        container_slot = 0,
        node = furnace.node,
        kind = furnace.kind,
        item = item_name,
        action_count = count,
        player_action = operation == "output" and "put" or "take",
    }
end

local function prepare_furnace_transfer(player, operation, requested_pos, item_name, requested_count)
    if operation ~= "input" and operation ~= "fuel" and operation ~= "output" then
        return nil, "invalid_operation"
    end
    if type(item_name) ~= "string" or item_name == ""
            or not minetest.registered_items[item_name] then
        return nil, "unknown_item"
    end
    if not requested_count or requested_count ~= math.floor(requested_count)
            or requested_count < 1 or requested_count > 99 then
        return nil, "invalid_count"
    end

    local furnace, status = resolve_furnace(player, requested_pos)
    if not furnace then
        return nil, status
    end
    if not furnace.accessible then
        return nil, furnace.access_status
    end
    local player_inv = player:get_inventory()
    if not player_inv then
        return nil, "no_inventory"
    end

    if operation == "output" then
        local stack = furnace.inv:get_stack("dst", 1)
        if stack:is_empty() or stack:get_name() ~= item_name then
            return nil, "item_not_found"
        end
        local wanted = math.min(requested_count, stack:get_count())
        local capacity = inventory_room_for_stack(player_inv, "main", stack, wanted)
        if capacity <= 0 then
            return nil, "inventory_full"
        end
        return prepared_furnace_action(
            furnace, operation, requested_pos, item_name, math.min(wanted, capacity)
        )
    end

    local list_name = operation == "input" and "src" or "fuel"
    local saw_item = false
    local saw_valid_item = false
    local rejection = nil
    for player_slot, stack in ipairs(player_inv:get_list("main") or {}) do
        if stack and not stack:is_empty() and stack:get_name() == item_name then
            saw_item = true
            local valid, invalid_status
            local replacement_fuel = false
            if operation == "input" then
                valid, invalid_status = furnace_recipe_for_stack(furnace, stack)
                if valid and not furnace.inv:room_for_item("dst", valid.item) then
                    valid = nil
                    invalid_status = "output_blocked"
                end
            else
                valid, invalid_status = furnace_fuel_for_stack(stack)
                replacement_fuel = valid and not valid.replacement:is_empty() or false
            end
            if valid then
                saw_valid_item = true
                local wanted = math.min(requested_count, stack:get_count())
                if replacement_fuel then
                    wanted = math.min(wanted, 1)
                    if not furnace.inv:get_stack("fuel", 1):is_empty() then
                        wanted = 0
                    end
                end
                local capacity = inventory_room_for_stack(
                    furnace.inv, list_name, stack, wanted
                )
                if capacity > 0 then
                    local prepared = prepared_furnace_action(
                        furnace, operation, requested_pos, item_name,
                        math.min(wanted, capacity)
                    )
                    prepared.player_slot = player_slot - 1
                    return prepared
                end
            else
                rejection = rejection or invalid_status
            end
        end
    end

    if not saw_item then
        return nil, "item_not_found"
    end
    if not saw_valid_item then
        return nil, rejection or (operation == "fuel" and "invalid_fuel" or "not_cookable")
    end
    return nil, operation == "fuel" and "fuel_full" or "input_full"
end

local function prepare_inventory_transfer_command(name, param, adapter, tag, prepare_transfer)
    local player = get_player(name)
    if not player then
        return command_result(name, tag, false, "no_player", "player not found")
    end

    local args = split_words(param)
    local nonce = args[1]
    local target = #args == 7 and parse_integer_position(args, 3) or nil
    local count = tonumber(args[7])
    if not valid_action_nonce(nonce) or not target then
        return command_result(
            name, tag, false, "invalid_parameters",
            "expected nonce operation x y z item count"
        )
    end

    local prepared, status = prepare_transfer(player, args[2], target, args[6], count)
    if not prepared then
        return command_result(
            name, tag, false, status, status,
            {
                nonce = nonce,
                operation = args[2],
                requested_target = position_array(target),
                item = args[6],
            }
        )
    end

    prepared.nonce = nonce
    local _, receipt_status = expect_inventory_action(name, adapter, nonce, prepared)
    if receipt_status then
        return command_result(
            name, tag, false, receipt_status, receipt_status,
            {
                nonce = nonce,
                operation = prepared.operation,
                requested_target = prepared.requested_target,
                item = prepared.item,
            }
        )
    end
    return command_result(name, tag, true, "prepared", "prepared", prepared)
end

local function build_observe(player, radius)
    local pos = player:get_pos()
    local node_pos = vector.round(pos)
    local facing = get_cardinal_facing(player)
    local obstacles = get_obstacles(node_pos, facing)

    local nodes = {}
    local node_candidates = {}
    local chest_candidates = {}
    local furnace_candidates = {}
    local node_limit = 200
    local node_name_limit = 24
    local scan_radius = radius
    local palette = {}
    local palette_ids = {}
    local voxel_runs = {}
    local last_palette_id = nil
    local last_run_length = 0
    local voxel_complete = true
    local resource_group_names = {
        "tree", "wood", "leaves", "flora", "plant", "stone", "material_stone",
        "soil", "sand", "ore", "blast_furnace_smeltable", "smoker_cookable",
    }
    local function resource_priority(node_name)
        if minetest.get_item_group(node_name, "tree") > 0 then
            return 0
        end
        if minetest.get_item_group(node_name, "ore") > 0
                or minetest.get_item_group(node_name, "blast_furnace_smeltable") > 0
                or node_name:find("ore", 1, true)
                or node_name:find("_with_", 1, true) then
            return 1
        end
        if minetest.get_item_group(node_name, "wood") > 0 then
            return 2
        end
        if minetest.get_item_group(node_name, "leaves") > 0 then
            return 3
        end
        if minetest.get_item_group(node_name, "soil") > 0 then
            return 8
        end
        if minetest.get_item_group(node_name, "sand") > 0 then
            return 9
        end
        return 4
    end
    local function palette_id_for(node_name)
        local existing = palette_ids[node_name]
        if existing then
            return existing, palette[existing]
        end
        local def = minetest.registered_nodes[node_name]
        local groups = {}
        for _, group in ipairs(resource_group_names) do
            if minetest.get_item_group(node_name, group) > 0 then
                groups[#groups + 1] = group
            end
        end
        local entry = {
            name = node_name,
            walkable = def and def.walkable == true or false,
            diggable = node_name ~= "air" and node_name ~= "ignore"
                and def ~= nil and def.diggable ~= false or false,
            groups = groups,
        }
        palette[#palette + 1] = entry
        local palette_id = #palette
        palette_ids[node_name] = palette_id
        return palette_id, entry
    end
    local function append_voxel(palette_id)
        if last_palette_id == palette_id then
            last_run_length = last_run_length + 1
            return
        end
        if last_palette_id then
            voxel_runs[#voxel_runs + 1] = { last_palette_id, last_run_length }
        end
        last_palette_id = palette_id
        last_run_length = 1
    end
    for y = -scan_radius, scan_radius do
        for x = -scan_radius, scan_radius do
            for z = -scan_radius, scan_radius do
                local p = {
                    x = node_pos.x + x,
                    y = node_pos.y + y,
                    z = node_pos.z + z,
                }
                local node = minetest.get_node_or_nil(p)
                local node_name = node and node.name or "ignore"
                if not node then
                    voxel_complete = false
                end
                local palette_id, palette_entry = palette_id_for(node_name)
                append_voxel(palette_id)
                if node_name ~= "air" and node_name ~= "ignore" then
                    if chest_node_kind(node_name) then
                        chest_candidates[#chest_candidates + 1] = {
                            pos = { x = p.x, y = p.y, z = p.z },
                            distance = vector.distance(node_pos, p),
                        }
                    end
                    if furnace_node_spec(node_name) then
                        furnace_candidates[#furnace_candidates + 1] = {
                            pos = { x = p.x, y = p.y, z = p.z },
                            distance = vector.distance(node_pos, p),
                        }
                    end
                    if palette_entry.walkable and palette_entry.diggable then
                        node_candidates[#node_candidates + 1] = {
                            pos = { p.x, p.y, p.z },
                            name = node_name,
                            groups = palette_entry.groups,
                            distance = vector.distance(node_pos, p),
                            priority = resource_priority(node_name),
                        }
                    end
                end
            end
        end
    end
    if last_palette_id then
        voxel_runs[#voxel_runs + 1] = { last_palette_id, last_run_length }
    end
    local diameter = scan_radius * 2 + 1
    local voxel_map = {
        radius = scan_radius,
        origin = {
            node_pos.x - scan_radius,
            node_pos.y - scan_radius,
            node_pos.z - scan_radius,
        },
        size = { diameter, diameter, diameter },
        order = "y_x_z_z_fastest",
        palette = palette,
        runs = voxel_runs,
        complete = voxel_complete,
    }
    table.sort(node_candidates, function(a, b)
        if a.priority ~= b.priority then
            return a.priority < b.priority
        end
        if a.distance ~= b.distance then
            return a.distance < b.distance
        end
        if a.name ~= b.name then
            return a.name < b.name
        end
        if a.pos[2] ~= b.pos[2] then
            return a.pos[2] < b.pos[2]
        end
        if a.pos[1] ~= b.pos[1] then
            return a.pos[1] < b.pos[1]
        end
        return a.pos[3] < b.pos[3]
    end)
    local node_total = #node_candidates
    local node_name_counts = {}
    for _, candidate in ipairs(node_candidates) do
        local count = node_name_counts[candidate.name] or 0
        if count < node_name_limit then
            nodes[#nodes + 1] = {
                pos = candidate.pos,
                name = candidate.name,
                groups = candidate.groups,
            }
            node_name_counts[candidate.name] = count + 1
            if #nodes >= node_limit then
                break
            end
        end
    end

    table.sort(chest_candidates, function(a, b)
        return a.distance < b.distance
    end)
    local resolved_chests = {}
    local seen_chests = {}
    for _, candidate in ipairs(chest_candidates) do
        local chest = resolve_chest(player, candidate.pos)
        if chest then
            local key = chest.pos.x .. ":" .. chest.pos.y .. ":" .. chest.pos.z
            if not seen_chests[key] then
                seen_chests[key] = true
                resolved_chests[#resolved_chests + 1] = chest
            end
        end
    end
    table.sort(resolved_chests, function(a, b)
        return a.distance < b.distance
    end)
    local chests = {}
    for idx = 1, math.min(#resolved_chests, CHEST_OBSERVATION_LIMIT) do
        chests[#chests + 1] = chest_observation(resolved_chests[idx])
    end

    table.sort(furnace_candidates, function(a, b)
        return a.distance < b.distance
    end)
    local resolved_furnaces = {}
    local seen_furnaces = {}
    for _, candidate in ipairs(furnace_candidates) do
        local key = candidate.pos.x .. ":" .. candidate.pos.y .. ":" .. candidate.pos.z
        if not seen_furnaces[key] then
            seen_furnaces[key] = true
            local furnace = resolve_furnace(player, candidate.pos)
            if furnace then
                resolved_furnaces[#resolved_furnaces + 1] = furnace
            end
        end
    end
    table.sort(resolved_furnaces, function(a, b)
        return a.distance < b.distance
    end)
    local furnaces = {}
    for idx = 1, math.min(#resolved_furnaces, FURNACE_OBSERVATION_LIMIT) do
        furnaces[#furnaces + 1] = furnace_observation(player, resolved_furnaces[idx])
    end

    local hostiles = {}
    local hostile_candidates = {}
    local mobs = {}
    local mob_candidates = {}
    local players = {}
    local dropped_items = {}
    local hostile_limit = 16
    local mob_limit = 16
    local entity_radius = math.max(radius + 2, 12)
    for _, obj in ipairs(minetest.get_objects_inside_radius(pos, entity_radius)) do
        if obj:is_player() then
            local player_name = obj:get_player_name()
            if player_name ~= player:get_player_name() and #players < 16 then
                local tpos = obj:get_pos()
                players[#players + 1] = {
                    type = "player",
                    name = player_name,
                    dx = math.floor(tpos.x - node_pos.x + 0.5),
                    dy = math.floor(tpos.y - node_pos.y + 0.5),
                    dz = math.floor(tpos.z - node_pos.z + 0.5),
                }
            end
        else
            local ent = obj:get_luaentity()
            if ent and ent.name then
                local ent_def = minetest.registered_entities[ent.name]
                local tpos = obj:get_pos()
                local hostile = is_hostile_entity(ent, ent_def)
                if hostile then
                    if tpos then
                        hostile_candidates[#hostile_candidates + 1] = {
                            type = "hostile",
                            name = ent.name,
                            hp = obj:get_hp(),
                            distance = vector.distance(pos, tpos),
                            dx = math.floor(tpos.x - node_pos.x + 0.5),
                            dy = math.floor(tpos.y - node_pos.y + 0.5),
                            dz = math.floor(tpos.z - node_pos.z + 0.5),
                        }
                    end
                elseif is_mob_entity(ent, ent_def) and tpos then
                    local food_mob = food_mob_metadata(obj, ent, ent_def)
                    mob_candidates[#mob_candidates + 1] = {
                        type = "mob",
                        name = ent.name,
                        category = entity_category(ent, ent_def),
                        hp = obj:get_hp(),
                        distance = vector.distance(pos, tpos),
                        dx = math.floor(tpos.x - node_pos.x + 0.5),
                        dy = math.floor(tpos.y - node_pos.y + 0.5),
                        dz = math.floor(tpos.z - node_pos.z + 0.5),
                        passive = food_mob.passive,
                        adult = food_mob.adult,
                        child = food_mob.child,
                        tamed = food_mob.tamed,
                        owned = food_mob.owned,
                        named = food_mob.named,
                        persistent = food_mob.persistent,
                        food_source = food_mob.food_source,
                        food_drops = food_mob.food_drops,
                        safe_to_hunt = food_mob.safe_to_hunt,
                        hunt_blocked_reason = food_mob.hunt_blocked_reason,
                    }
                end
                local item_string = ent.itemstring
                if type(item_string) == "string" and item_string ~= "" and #dropped_items < 16 then
                    local stack = ItemStack(item_string)
                    if not stack:is_empty() then
                        local tpos = obj:get_pos()
                        dropped_items[#dropped_items + 1] = {
                            type = "item",
                            name = stack:get_name(),
                            count = stack:get_count(),
                            dx = math.floor(tpos.x - node_pos.x + 0.5),
                            dy = math.floor(tpos.y - node_pos.y + 0.5),
                            dz = math.floor(tpos.z - node_pos.z + 0.5),
                        }
                    end
                end
            end
        end
    end
    table.sort(hostile_candidates, function(a, b)
        return a.distance < b.distance
    end)
    for idx = 1, math.min(#hostile_candidates, hostile_limit) do
        local hostile = hostile_candidates[idx]
        hostile.distance = math.floor(hostile.distance * 10 + 0.5) / 10
        hostiles[#hostiles + 1] = hostile
    end
    table.sort(mob_candidates, function(a, b)
        return a.distance < b.distance
    end)
    for idx = 1, math.min(#mob_candidates, mob_limit) do
        local mob = mob_candidates[idx]
        mob.distance = math.floor(mob.distance * 10 + 0.5) / 10
        mobs[#mobs + 1] = mob
    end

    local goal = ""
    local meta = player:get_meta()
    if meta then
        goal = meta:get_string("llm_goal") or ""
    end

    local inventory = {}
    local inv = player:get_inventory()
    if inv then
        local wield = player:get_wielded_item()
        inventory.wield = stack_observation(wield, true)
        local main = inv:get_list("main") or {}
        local items = {}
        local item_limit = 30
        for _, stack in ipairs(main) do
            if stack and not stack:is_empty() then
                items[#items + 1] = stack_observation(stack, false)
                if #items >= item_limit then
                    break
                end
            end
        end
        inventory.main = items
        inventory.main_truncated = #items >= item_limit
    end

    local hunger_available, hunger, saturation = hunger_observation(player)
    local crafting = craftable_observation(player)

    local data = {
        schema_version = 8,
        health = player:get_hp(),
        hunger_available = hunger_available,
        hunger = hunger,
        saturation = saturation,
        position = { node_pos.x, node_pos.y, node_pos.z },
        facing = facing,
        nodes = nodes,
        node_total = node_total,
        node_limit = node_limit,
        node_truncated = #nodes < node_total,
        voxel_map = voxel_map,
        inventory = inventory,
        chests = chests,
        chests_truncated = #resolved_chests > CHEST_OBSERVATION_LIMIT,
        furnaces = furnaces,
        furnaces_truncated = #resolved_furnaces > FURNACE_OBSERVATION_LIMIT,
        crafting = crafting,
        players = players,
        hostiles = hostiles,
        mobs = mobs,
        hostile_scan_radius = entity_radius,
        mob_scan_radius = entity_radius,
        hunt_policy = {
            min_remaining_adults = HUNT_MIN_REMAINING_ADULTS,
            max_strikes = HUNT_MAX_STRIKES,
            attack_range = HUNT_ATTACK_RANGE,
        },
        items = dropped_items,
        obstacles = obstacles,
        goal = goal,
    }

    if minetest.write_json then
        return minetest.write_json(data)
    end
    return minetest.serialize(data)
end

minetest.register_chatcommand("bot_observe", {
    params = "[radius]",
    description = "Return JSON of nearby nodes/entities/inventory",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return false, "player not found"
        end
        local radius = tonumber(param) or 2
        radius = math.floor(math.max(1, math.min(radius, 8)))
        local json = build_observe(player, radius)
        minetest.chat_send_player(name, "BOT_OBSERVE " .. json)
        return true, "ok"
    end,
})

minetest.register_chatcommand("bot_path_node", {
    params = "<node_name|group:name> [radius]",
    description = "Find a safe walking path near a node without moving the player",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_PATH_NODE", false, "no_player", "player not found")
        end
        local args = split_words(param)
        local selector = parse_node_selector(args[1])
        if not selector then
            return command_result(
                name, "BOT_PATH_NODE", false, "invalid_selector", "invalid node selector",
                { selector = args[1] or "" }
            )
        end
        local radius = clamp_integer(args[2], 16, PATH_RADIUS_MIN, PATH_RADIUS_MAX)
        if not pathfinder_available() then
            return command_result(
                name, "BOT_PATH_NODE", false, "pathfinder_unavailable",
                "server pathfinder unavailable", { selector = selector, radius = radius }
            )
        end
        local plan, status = plan_node_path(player, selector, radius, false)
        if not plan then
            return command_result(
                name, "BOT_PATH_NODE", false, status, status,
                { selector = selector, radius = radius }
            )
        end
        plan.radius = radius
        plan.arrival_action = { type = "none" }
        return command_result(name, "BOT_PATH_NODE", true, "path_found", "path found", plan)
    end,
})

minetest.register_chatcommand("bot_gather_path", {
    params = "<node_name|group:name> [count] [radius]",
    description = "Find a safe path to an unprotected diggable resource",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_GATHER_PATH", false, "no_player", "player not found")
        end
        local args = split_words(param)
        local selector = parse_node_selector(args[1])
        if not selector then
            return command_result(
                name, "BOT_GATHER_PATH", false, "invalid_selector", "invalid node selector",
                { selector = args[1] or "" }
            )
        end
        local count = clamp_integer(args[2], 1, 1, 8)
        local radius = clamp_integer(args[3], 16, PATH_RADIUS_MIN, PATH_RADIUS_MAX)
        if not pathfinder_available() then
            return command_result(
                name, "BOT_GATHER_PATH", false, "pathfinder_unavailable",
                "server pathfinder unavailable",
                { selector = selector, count = count, radius = radius }
            )
        end
        local plan, status = plan_node_path(player, selector, radius, true)
        if not plan then
            return command_result(
                name, "BOT_GATHER_PATH", false, status, status,
                { selector = selector, count = count, radius = radius }
            )
        end
        plan.radius = radius
        plan.arrival_action = {
            type = "collect",
            node = plan.node,
            count = count,
            radius = 6,
        }
        return command_result(name, "BOT_GATHER_PATH", true, "path_found", "path found", plan)
    end,
})

local function hunt_time()
    return tonumber(minetest.get_gametime()) or 0
end

local function validate_food_hunt_target(obj, expected_name, expected_entity)
    if not obj then
        return nil, "target_gone"
    end
    local pos_ok, pos = pcall(function()
        return obj:get_pos()
    end)
    if not pos_ok or not pos then
        return nil, "target_gone"
    end
    local player_ok, is_player = pcall(function()
        return obj:is_player()
    end)
    if not player_ok or is_player then
        return nil, "target_gone"
    end
    local entity_ok, ent = pcall(function()
        return obj:get_luaentity()
    end)
    if not entity_ok or not ent or ent.name ~= expected_name
            or (expected_entity and ent ~= expected_entity) then
        return nil, "target_gone"
    end
    local ent_def = minetest.registered_entities[ent.name]
    local metadata = food_mob_metadata(obj, ent, ent_def)
    if not metadata.safe_to_hunt then
        return {
            object = obj,
            entity = ent,
            position = pos,
            metadata = metadata,
        }, "target_unsafe"
    end
    return {
        object = obj,
        entity = ent,
        position = pos,
        metadata = metadata,
    }, nil
end

local function eligible_food_population(pos, radius, entity_name)
    local count = 0
    for _, obj in ipairs(minetest.get_objects_inside_radius(pos, radius)) do
        if not obj:is_player() then
            local ent = obj:get_luaentity()
            if ent and ent.name == entity_name then
                local ent_def = minetest.registered_entities[ent.name]
                local metadata = food_mob_metadata(obj, ent, ent_def)
                if metadata.safe_to_hunt then
                    count = count + 1
                end
            end
        end
    end
    return count
end

local function hunt_plan_payload(task, target_info, path, stand, distance)
    return {
        hunt_id = task.id,
        target = task.entity_name,
        target_position = position_array(vector.round(target_info.position)),
        stand = position_array(stand),
        path = path,
        waypoint_count = #path,
        distance = math.floor(distance * 10 + 0.5) / 10,
        food_drops = target_info.metadata.food_drops,
        population = task.population,
        min_remaining_adults = HUNT_MIN_REMAINING_ADULTS,
        arrival_action = { type = "hunt", hunt_id = task.id },
    }
end

local function find_food_hunt_plan(player, requested_name, radius)
    local player_pos = player:get_pos()
    local candidates = {}
    local nearest_unsafe = nil
    for _, obj in ipairs(minetest.get_objects_inside_radius(player_pos, radius)) do
        if not obj:is_player() then
            local ent = obj:get_luaentity()
            if ent and ent.name
                    and (not requested_name or ent.name == requested_name) then
                local ent_def = minetest.registered_entities[ent.name]
                local metadata = food_mob_metadata(obj, ent, ent_def)
                local target_pos = obj:get_pos()
                if target_pos then
                    if metadata.safe_to_hunt then
                        candidates[#candidates + 1] = {
                            object = obj,
                            entity = ent,
                            position = target_pos,
                            metadata = metadata,
                            distance = vector.distance(player_pos, target_pos),
                        }
                    elseif requested_name and not nearest_unsafe then
                        nearest_unsafe = metadata.hunt_blocked_reason
                    end
                end
            end
        end
    end
    table.sort(candidates, function(a, b)
        return a.distance < b.distance
    end)
    local saw_small_population = false
    local saw_unreachable = false
    for _, candidate in ipairs(candidates) do
        local population = eligible_food_population(
            candidate.position,
            radius,
            candidate.entity.name
        )
        if population > HUNT_MIN_REMAINING_ADULTS then
            local path, stand = plan_path_near(
                player,
                candidate.position,
                HUNT_ATTACK_RANGE - 0.5,
                24,
                math.min(PATH_RADIUS_MAX, radius + 6)
            )
            if path then
                local task = {
                    id = next_hunt_id,
                    object = candidate.object,
                    entity_name = candidate.entity.name,
                    entity_ref = candidate.entity,
                    radius = radius,
                    population = population,
                    expires_at = hunt_time() + HUNT_EXPIRY_SECONDS,
                    running = false,
                    total_strikes = 0,
                }
                next_hunt_id = next_hunt_id + 1
                if next_hunt_id > 2147483647 then
                    next_hunt_id = 1
                end
                return task, candidate, path, stand, nil
            end
            saw_unreachable = true
        else
            saw_small_population = true
        end
    end
    if saw_small_population then
        return nil, nil, nil, nil, "population_too_small"
    end
    if saw_unreachable then
        return nil, nil, nil, nil, "no_reachable_target"
    end
    if nearest_unsafe then
        return nil, nil, nil, nil, "target_unsafe:" .. nearest_unsafe
    end
    if requested_name then
        return nil, nil, nil, nil, "target_not_found"
    end
    return nil, nil, nil, nil, "no_eligible_food_mob"
end

minetest.register_chatcommand("bot_hunt_path", {
    params = "[entity_name|auto] [radius]",
    description = "Plan a path to one eligible passive food animal",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_HUNT_PATH", false, "no_player", "player not found")
        end
        local existing = hunt_targets[name]
        if existing and existing.running then
            return command_result(
                name, "BOT_HUNT_PATH", false, "hunt_in_progress", "hunt already in progress",
                { hunt_id = existing.id, target = existing.entity_name }
            )
        end
        if not pathfinder_available() then
            return command_result(
                name, "BOT_HUNT_PATH", false, "pathfinder_unavailable",
                "server pathfinder unavailable"
            )
        end
        local args = split_words(param)
        local requested_name = args[1]
        local radius_arg = args[2]
        if requested_name and tonumber(requested_name) then
            radius_arg = requested_name
            requested_name = nil
        elseif requested_name == "" or requested_name == "auto" then
            requested_name = nil
        end
        if requested_name and not minetest.registered_entities[requested_name] then
            return command_result(
                name, "BOT_HUNT_PATH", false, "invalid_target", "unknown entity",
                { target = requested_name }
            )
        end
        local radius = clamp_integer(radius_arg, 16, PATH_RADIUS_MIN, HUNT_RADIUS_MAX)
        local task, target_info, path, stand, status = find_food_hunt_plan(
            player,
            requested_name,
            radius
        )
        if not task then
            local blocked_reason = string.match(status or "", "^target_unsafe:(.+)$")
            if blocked_reason then
                status = "target_unsafe"
            end
            return command_result(
                name, "BOT_HUNT_PATH", false, status, status,
                {
                    target = requested_name or "auto",
                    radius = radius,
                    hunt_blocked_reason = blocked_reason,
                }
            )
        end
        hunt_targets[name] = task
        local payload = hunt_plan_payload(task, target_info, path, stand, target_info.distance)
        payload.radius = radius
        return command_result(name, "BOT_HUNT_PATH", true, "path_found", "path found", payload)
    end,
})

local function finish_hunt(name, task, ok, status, message, payload, clear_task)
    if hunt_targets[name] ~= task then
        return
    end
    task.running = false
    payload = payload or {}
    payload.hunt_id = task.id
    payload.target = task.entity_name
    payload.strikes = task.total_strikes
    if clear_task then
        hunt_targets[name] = nil
    end
    command_result(name, "BOT_HUNT", ok, status, message, payload)
end

local function finish_hunt_with_repath(name, task, player, target_info)
    local path, stand = plan_path_near(
        player,
        target_info.position,
        HUNT_ATTACK_RANGE - 0.5,
        32,
        math.min(PATH_RADIUS_MAX, task.radius + 6)
    )
    if not path then
        finish_hunt(
            name, task, false, "no_path", "target moved out of reach and no path was found",
            { target_position = position_array(vector.round(target_info.position)) }, false
        )
        return
    end
    task.expires_at = hunt_time() + HUNT_EXPIRY_SECONDS
    finish_hunt(
        name, task, true, "repath_required", "target moved; follow the replacement path",
        {
            target_position = position_array(vector.round(target_info.position)),
            stand = position_array(stand),
            path = path,
            waypoint_count = #path,
            food_drops = target_info.metadata.food_drops,
            arrival_action = { type = "hunt", hunt_id = task.id },
        }, false
    )
end

local function run_hunt_step(name, task)
    if hunt_targets[name] ~= task or not task.running then
        return
    end
    local player = get_player(name)
    if not player then
        task.running = false
        hunt_targets[name] = nil
        return
    end
    if hunt_time() > task.expires_at then
        finish_hunt(name, task, false, "hunt_expired", "hunt expired", nil, true)
        return
    end
    local target_info, target_error = validate_food_hunt_target(
        task.object,
        task.entity_name,
        task.entity_ref
    )
    if not target_info then
        local killed = task.total_strikes > 0 and target_error == "target_gone"
        finish_hunt(
            name, task, killed, killed and "killed" or target_error,
            killed and "target killed" or target_error, nil, true
        )
        return
    end
    if target_error then
        finish_hunt(
            name, task, false, target_error, target_error,
            { hunt_blocked_reason = target_info.metadata.hunt_blocked_reason }, true
        )
        return
    end
    local population = eligible_food_population(
        target_info.position,
        task.radius,
        task.entity_name
    )
    if population <= HUNT_MIN_REMAINING_ADULTS then
        finish_hunt(
            name, task, false, "population_too_small", "not enough eligible adults remain",
            {
                population = population,
                min_remaining_adults = HUNT_MIN_REMAINING_ADULTS,
            }, true
        )
        return
    end
    local distance = vector.distance(player:get_pos(), target_info.position)
    if distance > HUNT_ATTACK_RANGE then
        finish_hunt_with_repath(name, task, player, target_info)
        return
    end
    if task.total_strikes >= HUNT_MAX_STRIKES then
        finish_hunt(
            name, task, false, "attack_incomplete", "target survived the strike limit",
            {
                target_hp = task.object:get_hp(),
                distance = math.floor(distance * 10 + 0.5) / 10,
            }, true
        )
        return
    end
    local call_ok, hit_ok, hit_message = pcall(punch_target, player, task.object)
    if not call_ok or not hit_ok then
        finish_hunt(
            name, task, false, "attack_failed",
            call_ok and hit_message or "target punch raised an error", nil, true
        )
        return
    end
    task.total_strikes = task.total_strikes + 1
    local pos_ok, target_pos = pcall(function()
        return task.object:get_pos()
    end)
    local hp_ok, target_hp = pcall(function()
        return task.object:get_hp()
    end)
    if not pos_ok or not target_pos or (hp_ok and target_hp <= 0) then
        finish_hunt(
            name, task, true, "killed", "target killed",
            { target_hp = hp_ok and target_hp or 0, food_drops = target_info.metadata.food_drops }, true
        )
        return
    end
    minetest.after(HUNT_STRIKE_INTERVAL, function()
        run_hunt_step(name, task)
    end)
end

minetest.register_chatcommand("bot_hunt", {
    params = "<hunt_id>",
    description = "Safely finish the one passive food animal selected by bot_hunt_path",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_HUNT", false, "no_player", "player not found")
        end
        local requested_id = tonumber((param or ""):match("^%s*(%d+)%s*$"))
        if not requested_id then
            return command_result(name, "BOT_HUNT", false, "invalid_hunt_id", "invalid hunt id")
        end
        local task = hunt_targets[name]
        if not task or task.id ~= requested_id then
            return command_result(
                name, "BOT_HUNT", false, "hunt_not_found", "hunt not found",
                { hunt_id = requested_id }
            )
        end
        if task.running then
            return command_result(
                name, "BOT_HUNT", false, "hunt_in_progress", "hunt already in progress",
                { hunt_id = task.id, target = task.entity_name }
            )
        end
        if hunt_time() > task.expires_at then
            hunt_targets[name] = nil
            return command_result(
                name, "BOT_HUNT", false, "hunt_expired", "hunt expired",
                { hunt_id = task.id, target = task.entity_name }
            )
        end
        task.running = true
        minetest.after(0, function()
            run_hunt_step(name, task)
        end)
        return true, "hunt started"
    end,
})

minetest.register_chatcommand("bot_attack_mobs", {
    params = "[radius]",
    description = "Punch nearest mob within radius",
    privs = { interact = true },
    func = function(name, param)
        local attacker = get_player(name)
        if not attacker then
            return command_result(name, "BOT_DEFEND", false, "no_attacker", "attacker not found")
        end
        local radius = tonumber(param) or 6
        radius = math.max(1, math.min(radius, 20))
        local pos = attacker:get_pos()
        local nearest = nil
        local nearest_dist = radius + 1
        for _, obj in ipairs(minetest.get_objects_inside_radius(pos, radius)) do
            if not obj:is_player() then
                local ent = obj:get_luaentity()
                if ent and ent.name then
                    local ent_def = minetest.registered_entities[ent.name]
                    local d = vector.distance(pos, obj:get_pos())
                    if is_hostile_entity(ent, ent_def) and d < nearest_dist then
                        nearest = obj
                        nearest_dist = d
                    end
                end
            end
        end
        if not nearest then
            return command_result(name, "BOT_DEFEND", false, "no_hostiles", "no mobs in range")
        end
        local ok, msg = punch_target(attacker, nearest)
        if ok then
            -- Finish a short, human-paced defensive combo without requiring another
            -- slow LLM round trip for every individual punch.
            for strike = 1, 2 do
                minetest.after(strike * 0.65, function()
                    local current_attacker = get_player(name)
                    local target_pos = nearest and nearest:get_pos()
                    local attacker_pos = current_attacker and current_attacker:get_pos()
                    if current_attacker and target_pos and attacker_pos
                            and vector.distance(attacker_pos, target_pos) <= radius then
                        punch_target(current_attacker, nearest)
                    end
                end)
            end
            minetest.log("action", "[llm_bot] " .. name .. " attacked nearest hostile mob")
        end
        return command_result(
            name, "BOT_DEFEND", ok, ok and "defended" or "attack_failed", msg,
            { distance = nearest_dist }
        )
    end,
})

local function find_target_object(name, radius, pos)
    local player = get_player(name)
    if player then
        return player
    end
    local best = nil
    local best_dist = radius + 1
    for _, obj in ipairs(minetest.get_objects_inside_radius(pos, radius)) do
        if obj:is_player() then
            if obj:get_player_name() == name then
                return obj
            end
        else
            local ent = obj:get_luaentity()
            if ent and ent.name and string.find(ent.name, name, 1, true) then
                local d = vector.distance(pos, obj:get_pos())
                if d < best_dist then
                    best = obj
                    best_dist = d
                end
            end
        end
    end
    return best
end

minetest.register_chatcommand("bot_approach", {
    params = "<name> [radius]",
    description = "Move close to a target player/entity",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_APPROACH", false, "no_player", "player not found")
        end
        local args = param:split(" ")
        local target_name = args[1]
        if not target_name or target_name == "" then
            return command_result(name, "BOT_APPROACH", false, "missing_target", "missing target")
        end
        local radius = tonumber(args[2]) or 20
        radius = math.max(1, math.min(radius, 50))
        local obj = find_target_object(target_name, radius, player:get_pos())
        if not obj then
            return command_result(
                name, "BOT_APPROACH", false, "target_not_found", "target not found",
                { target = target_name }
            )
        end
        local pos = obj:get_pos()
        player:set_pos({ x = pos.x, y = pos.y, z = pos.z })
        return command_result(
            name, "BOT_APPROACH", true, "approached", nil,
            { target = target_name }
        )
    end,
})

minetest.register_chatcommand("bot_interact", {
    params = "<name> [radius]",
    description = "Interact with a target player/entity",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_INTERACT", false, "no_player", "player not found")
        end
        local args = param:split(" ")
        local target_name = args[1]
        if not target_name or target_name == "" then
            return command_result(name, "BOT_INTERACT", false, "missing_target", "missing target")
        end
        local radius = tonumber(args[2]) or 6
        radius = math.max(1, math.min(radius, 20))
        local obj = find_target_object(target_name, radius, player:get_pos())
        if not obj then
            return command_result(
                name, "BOT_INTERACT", false, "target_not_found", "target not found",
                { target = target_name }
            )
        end
        obj:right_click(player)
        return command_result(
            name, "BOT_INTERACT", true, "interacted", nil,
            { target = target_name }
        )
    end,
})

minetest.register_chatcommand("bot_fight", {
    params = "<name> [radius]",
    description = "Attack a target player/entity",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(name, "BOT_FIGHT", false, "no_player", "player not found")
        end
        local args = param:split(" ")
        local target_name = args[1]
        if not target_name or target_name == "" then
            return command_result(name, "BOT_FIGHT", false, "missing_target", "missing target")
        end
        local radius = tonumber(args[2]) or 6
        radius = math.max(1, math.min(radius, 20))
        local obj = find_target_object(target_name, radius, player:get_pos())
        if not obj then
            return command_result(
                name, "BOT_FIGHT", false, "target_not_found", "target not found",
                { target = target_name }
            )
        end
        local ok, msg = punch_target(player, obj)
        return command_result(
            name, "BOT_FIGHT", ok, ok and "attacked" or "attack_failed", msg,
            { target = target_name }
        )
    end,
})

local function find_nearby_bed(pos, radius)
    local minp = vector.subtract(pos, radius)
    local maxp = vector.add(pos, radius)
    local nodes = minetest.find_nodes_in_area(minp, maxp, {"group:bed", "mcl_beds:*"})
    if #nodes == 0 then
        return nil
    end
    table.sort(nodes, function(a, b)
        return vector.distance(pos, a) < vector.distance(pos, b)
    end)
    return nodes[1]
end

minetest.register_chatcommand("bot_sleep", {
    params = "[radius]",
    description = "Sleep in the nearest bed",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            minetest.chat_send_player(name, "BOT_SLEEP {\"ok\":false,\"status\":\"no_player\"}")
            return false, "player not found"
        end
        local radius = tonumber(param) or 6
        radius = math.max(1, math.min(radius, 20))
        local pos = player:get_pos()
        local bed_pos = find_nearby_bed(pos, radius)
        if not bed_pos then
            minetest.chat_send_player(name, "BOT_SLEEP {\"ok\":false,\"status\":\"no_bed\"}")
            return false, "no bed in range"
        end
        local node = minetest.get_node(bed_pos)
        if minetest.get_modpath("mcl_beds") and mcl_beds and mcl_beds.on_rightclick then
            mcl_beds.on_rightclick(bed_pos, player, string.sub(node.name, -4) == "_top")
            minetest.chat_send_player(name, "BOT_SLEEP {\"ok\":true,\"status\":\"sleep\"}")
            return true, "sleep"
        end
        local def = minetest.registered_nodes[node.name]
        if def and def.on_rightclick then
            def.on_rightclick(bed_pos, node, player, player:get_wielded_item(), nil)
            minetest.chat_send_player(name, "BOT_SLEEP {\"ok\":true,\"status\":\"sleep\"}")
            return true, "sleep"
        end
        minetest.chat_send_player(name, "BOT_SLEEP {\"ok\":false,\"status\":\"failed\"}")
        return false, "bed interaction failed"
    end,
})

minetest.register_chatcommand("bot_prepare_mine", {
    params = "[x y z]",
    description = "Prepare a native dig by validating the node and wielding the best tool",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "no_player",
                "player not found"
            )
        end

        local pos = player:get_pos()
        if not pos then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "no_position",
                "player position unavailable"
            )
        end
        local trimmed = (param or ""):gsub("^%s+", ""):gsub("%s+$", "")
        local target
        if trimmed == "" then
            target = front_pos(player)
        else
            target = parse_pos_params(trimmed)
            if not target then
                return command_result(
                    name,
                    "BOT_MINE_PREPARE",
                    false,
                    "invalid_position",
                    "expected x y z"
                )
            end
            target = vector.round(target)
        end

        local target_array = position_array(target)
        if vector.distance(pos, target) > 6 then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "out_of_range",
                "target is out of range",
                { target = target_array }
            )
        end
        if minetest.is_protected(target, name) then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "protected",
                "target is protected",
                { target = target_array }
            )
        end

        local node = minetest.get_node_or_nil(target)
        if not node then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "unloaded",
                "target node is not loaded",
                { target = target_array }
            )
        end
        if node.name == "air" or node.name == "ignore" then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "no_block",
                "target has no diggable block",
                { target = target_array, node = node.name }
            )
        end

        local def = minetest.registered_nodes[node.name]
        local allowed, denied_status = node_allows_dig(def, target, player)
        if not allowed then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                denied_status,
                "node does not allow digging",
                { target = target_array, node = node.name }
            )
        end

        local inv = player:get_inventory()
        if not inv then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "no_inventory",
                "player inventory unavailable",
                { target = target_array, node = node.name }
            )
        end
        local candidate, candidate_error = fastest_dig_candidate(
            player,
            inv,
            node.name,
            def.groups or {}
        )
        if not candidate then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                candidate_error,
                "no inventory or hand tool can dig this node",
                { target = target_array, node = node.name }
            )
        end

        local wielded, wield_error = wield_inventory_slot(
            player,
            inv,
            candidate.slot,
            candidate.stack
        )
        if not wielded then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "wield_failed",
                wield_error,
                { target = target_array, node = node.name, tool = candidate.tool }
            )
        end

        local wield_index = player:get_wield_index()
        if type(wield_index) ~= "number" or wield_index < 1 then
            return command_result(
                name,
                "BOT_MINE_PREPARE",
                false,
                "invalid_wield_index",
                "player wield index unavailable",
                { target = target_array, node = node.name, tool = candidate.tool }
            )
        end
        local above = adjacent_node_toward(target, player_eye_position(player, pos))
        return command_result(
            name,
            "BOT_MINE_PREPARE",
            true,
            "prepared",
            "native dig prepared",
            {
                target = target_array,
                above = position_array(above),
                wield_index = math.floor(wield_index) - 1,
                dig_time = candidate.dig_time,
                node = node.name,
                tool = candidate.tool,
                harvestable = candidate.harvestable,
            }
        )
    end,
})

local function parse_mine_verification(param)
    local args = split_words(param)
    if #args ~= 4 then
        return nil
    end
    local x = tonumber(args[1])
    local y = tonumber(args[2])
    local z = tonumber(args[3])
    if not x or not y or not z or args[4] == "" then
        return nil
    end
    return vector.round({ x = x, y = y, z = z }), args[4]
end

minetest.register_chatcommand("bot_verify_mine", {
    params = "<x y z expected_node>",
    description = "Verify that a native dig changed its expected target node",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name,
                "BOT_MINE_VERIFY",
                false,
                "no_player",
                "player not found"
            )
        end
        local target, expected_node = parse_mine_verification(param)
        if not target then
            return command_result(
                name,
                "BOT_MINE_VERIFY",
                false,
                "invalid_parameters",
                "expected x y z expected_node"
            )
        end

        local current = minetest.get_node_or_nil(target)
        local current_node = current and current.name or "unloaded"
        local changed = current ~= nil and current_node ~= expected_node
        local status = changed and "changed" or (current and "unchanged" or "unloaded")
        return command_result(
            name,
            "BOT_MINE_VERIFY",
            changed,
            status,
            status,
            {
                target = position_array(target),
                expected_node = expected_node,
                current_node = current_node,
            }
        )
    end,
})

minetest.register_chatcommand("bot_chest_inspect", {
    params = "<x y z>",
    description = "Inspect one nearby normal, trapped, or shulker chest",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name, "BOT_CHEST_INSPECT", false, "no_player", "player not found"
            )
        end
        local args = split_words(param)
        local target = #args == 3 and parse_integer_position(args, 1) or nil
        if not target then
            return command_result(
                name, "BOT_CHEST_INSPECT", false, "invalid_parameters", "expected integer x y z"
            )
        end
        local chest, status = resolve_chest(player, target)
        if not chest then
            return command_result(name, "BOT_CHEST_INSPECT", false, status, status)
        end
        local observation = chest_observation(chest)
        observation.requested_target = position_array(target)
        if not chest.accessible then
            return command_result(
                name, "BOT_CHEST_INSPECT", false, chest.access_status, chest.access_status,
                observation
            )
        end
        return command_result(
            name, "BOT_CHEST_INSPECT", true, "inspected", "inspected", observation
        )
    end,
})

minetest.register_chatcommand("bot_chest_prepare", {
    params = "<nonce> <deposit|withdraw> <x y z> <item_name> <count>",
    description = "Prepare one callback-safe native chest inventory move",
    privs = { interact = true },
    func = function(name, param)
        return prepare_inventory_transfer_command(
            name, param, "chest", "BOT_CHEST_PREPARE", prepare_chest_transfer
        )
    end,
})

minetest.register_chatcommand("bot_chest_receipt", {
    params = "<nonce>",
    description = "Read the exact result of one prepared native chest inventory move",
    privs = { interact = true },
    func = function(name, param)
        return report_inventory_transfer_receipt(
            name, param, "chest", "BOT_CHEST_RECEIPT"
        )
    end,
})

minetest.register_chatcommand("bot_chest_cancel", {
    params = "<nonce>",
    description = "Cancel one prepared native chest inventory move",
    privs = { interact = true },
    func = function(name, param)
        return cancel_inventory_transfer(name, param, "chest", "BOT_CHEST_CANCEL")
    end,
})

minetest.register_chatcommand("bot_craft_prepare", {
    params = "<nonce> <item_name> <minimum_output_count>",
    description = "Prepare one bounded native basic crafting operation",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "no_player", "player not found"
            )
        end
        local args = split_words(param)
        local nonce = args[1]
        local item_name = args[2]
        local requested = tonumber(args[3])
        if #args ~= 3 or not valid_action_nonce(nonce)
                or type(item_name) ~= "string" or item_name == ""
                or not requested or requested ~= math.floor(requested)
                or requested < 1 or requested > CRAFT_MAX_OUTPUT_COUNT then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "invalid_parameters",
                "expected nonce item minimum_output_count (1-64)",
                { nonce = nonce, item = item_name }
            )
        end
        local pending, pending_status = active_inventory_receipt(name)
        if pending then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "action_pending", "action pending",
                { nonce = nonce, item = item_name, pending_adapter = pending.adapter }
            )
        elseif pending_status == "expired" then
            reset_crafting_grid(player)
        end

        local inv = player:get_inventory()
        if not inv then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "no_inventory", "no inventory",
                { nonce = nonce, item = item_name }
            )
        end
        if not inv:is_empty("craftresult") then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "craft_result_busy",
                "collect or clear the existing craft result first",
                { nonce = nonce, item = item_name }
            )
        end
        if not inv:is_empty("craft") then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, "craft_grid_busy",
                "clear the existing craft grid first",
                { nonce = nonce, item = item_name }
            )
        end
        reset_crafting_grid(player)

        local plan, status = find_craft_plan(
            player, item_name, requested, false, nearby_crafting_table(player)
        )
        if not plan then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, status, status,
                { nonce = nonce, item = item_name, requested = requested }
            )
        end
        local activated, activate_status = activate_craft_grid(player, plan)
        if not activated then
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, activate_status, activate_status,
                { nonce = nonce, item = item_name, requested = requested }
            )
        end

        local receipt, receipt_status = expect_craft_action(
            name, nonce, player, plan, requested
        )
        if not receipt then
            reset_crafting_grid(player)
            return command_result(
                name, "BOT_CRAFT_PREPARE", false, receipt_status, receipt_status,
                { nonce = nonce, item = item_name, requested = requested }
            )
        end
        return command_result(
            name, "BOT_CRAFT_PREPARE", true, "prepared", "prepared",
            {
                nonce = nonce,
                item = plan.item,
                requested = requested,
                batches = plan.batches,
                produced = plan.produced,
                output_per_batch = plan.output_per_batch,
                grid_size = plan.grid_size,
                used_table = plan.table_required,
                table_pos = plan.table_pos,
                moves = plan.moves,
                ingredients = plan.ingredients,
            }
        )
    end,
})

minetest.register_chatcommand("bot_craft_receipt", {
    params = "<nonce>",
    description = "Read and verify one native crafting operation",
    privs = { interact = true },
    func = function(name, param)
        local nonce = parse_action_nonce(param)
        if not nonce then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, "invalid_parameters", "expected nonce"
            )
        end
        local receipt, status = active_adapter_receipt(name, "craft")
        if not receipt then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, status, status, { nonce = nonce }
            )
        end
        if receipt.nonce ~= nonce then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, "nonce_mismatch", "nonce mismatch",
                { nonce = nonce }
            )
        end

        local details = craft_receipt_details(receipt)
        if receipt.error then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, receipt.error, receipt.error, details
            )
        end
        if receipt.phase ~= "verify" then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, "pending", "pending", details
            )
        end

        local player = get_player(name)
        local inv = player and player:get_inventory() or nil
        if not inv then
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false, "no_inventory", "no inventory", details
            )
        end
        if not inv:is_empty("craft") or not inv:is_empty("craftresult") then
            receipt.error = "craft_cleanup_incomplete"
            receipt.phase = "error"
            details = craft_receipt_details(receipt)
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false,
                "craft_cleanup_incomplete", "craft cleanup incomplete", details
            )
        end
        local current_count = inventory_main_item_count(inv, receipt.item)
        if current_count < receipt.initial_main_count + receipt.delivered then
            receipt.error = "crafted_output_not_confirmed"
            receipt.phase = "error"
            details = craft_receipt_details(receipt)
            return command_result(
                name, "BOT_CRAFT_RECEIPT", false,
                "crafted_output_not_confirmed", "crafted output not confirmed", details
            )
        end

        receipt.completed = true
        receipt.phase = "completed"
        details = craft_receipt_details(receipt)
        details.grid_reset = reset_crafting_grid(player)
        inventory_action_receipts[name] = nil
        return command_result(
            name, "BOT_CRAFT_RECEIPT", true, "completed", "completed", details
        )
    end,
})

minetest.register_chatcommand("bot_craft_cancel", {
    params = "<nonce>",
    description = "Cancel one prepared native crafting operation without deleting items",
    privs = { interact = true },
    func = function(name, param)
        local nonce = parse_action_nonce(param)
        if not nonce then
            return command_result(
                name, "BOT_CRAFT_CANCEL", false, "invalid_parameters", "expected nonce"
            )
        end
        local receipt, status = active_adapter_receipt(name, "craft")
        if not receipt then
            return command_result(
                name, "BOT_CRAFT_CANCEL", false, status, status, { nonce = nonce }
            )
        end
        if receipt.nonce ~= nonce then
            return command_result(
                name, "BOT_CRAFT_CANCEL", false, "nonce_mismatch", "nonce mismatch",
                { nonce = nonce }
            )
        end

        local player = get_player(name)
        local inv = player and player:get_inventory() or nil
        local cleanup_required = inv ~= nil and not crafting_lists_empty(inv)
        local details = craft_receipt_details(receipt)
        details.cleanup_required = cleanup_required
        details.grid_reset = not cleanup_required and reset_crafting_grid(player) or false
        inventory_action_receipts[name] = nil
        local cancel_status = cleanup_required
            and "canceled_cleanup_required" or "canceled"
        return command_result(
            name, "BOT_CRAFT_CANCEL", true, cancel_status, cancel_status, details
        )
    end,
})

minetest.register_chatcommand("bot_furnace_inspect", {
    params = "<x y z>",
    description = "Inspect one nearby furnace, blast furnace, or smoker",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name, "BOT_FURNACE_INSPECT", false, "no_player", "player not found"
            )
        end
        local args = split_words(param)
        local target = #args == 3 and parse_integer_position(args, 1) or nil
        if not target then
            return command_result(
                name, "BOT_FURNACE_INSPECT", false, "invalid_parameters",
                "expected integer x y z"
            )
        end
        local furnace, status = resolve_furnace(player, target)
        if not furnace then
            return command_result(name, "BOT_FURNACE_INSPECT", false, status, status)
        end
        local observation = furnace_observation(player, furnace)
        observation.requested_target = position_array(target)
        if not furnace.accessible then
            return command_result(
                name, "BOT_FURNACE_INSPECT", false,
                furnace.access_status, furnace.access_status, observation
            )
        end
        return command_result(
            name, "BOT_FURNACE_INSPECT", true, "inspected", "inspected", observation
        )
    end,
})

minetest.register_chatcommand("bot_furnace_prepare", {
    params = "<nonce> <input|fuel|output> <x y z> <item_name> <count>",
    description = "Prepare one callback-safe native furnace inventory move",
    privs = { interact = true },
    func = function(name, param)
        return prepare_inventory_transfer_command(
            name, param, "furnace", "BOT_FURNACE_PREPARE", prepare_furnace_transfer
        )
    end,
})

minetest.register_chatcommand("bot_furnace_receipt", {
    params = "<nonce>",
    description = "Read the exact result of one prepared native furnace inventory move",
    privs = { interact = true },
    func = function(name, param)
        return report_inventory_transfer_receipt(
            name, param, "furnace", "BOT_FURNACE_RECEIPT"
        )
    end,
})

minetest.register_chatcommand("bot_furnace_cancel", {
    params = "<nonce>",
    description = "Cancel one prepared native furnace inventory move",
    privs = { interact = true },
    func = function(name, param)
        return cancel_inventory_transfer(name, param, "furnace", "BOT_FURNACE_CANCEL")
    end,
})

-- Retained for manual and older external clients. The current Rust client uses
-- chest inspection plus the prepare/receipt protocol instead.
minetest.register_chatcommand("bot_chest_verify", {
    params = "<x y z> <item_name>",
    description = "Read the current count of an item in a nearby supported chest",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return command_result(
                name, "BOT_CHEST_VERIFY", false, "no_player", "player not found"
            )
        end
        local args = split_words(param)
        local target = #args == 4 and parse_integer_position(args, 1) or nil
        local item_name = args[4]
        if not target or not item_name or not minetest.registered_items[item_name] then
            return command_result(
                name, "BOT_CHEST_VERIFY", false, "invalid_parameters",
                "expected integer x y z and an exact item name"
            )
        end
        local chest, status = resolve_chest(player, target)
        if not chest then
            return command_result(name, "BOT_CHEST_VERIFY", false, status, status)
        end
        if not chest.accessible then
            return command_result(
                name, "BOT_CHEST_VERIFY", false, chest.access_status, chest.access_status,
                {
                    requested_target = position_array(target),
                    target = position_array(chest.pos),
                    item = item_name,
                }
            )
        end
        return command_result(
            name, "BOT_CHEST_VERIFY", true, "verified", "verified",
            {
                requested_target = position_array(target),
                target = position_array(chest.pos),
                item = item_name,
                chest_count = inventory_item_count(chest.inventories, item_name),
            }
        )
    end,
})

-- Retained for manual and older external clients. Current API mining uses the
-- native player dig protocol via bot_prepare_mine and bot_verify_mine.
minetest.register_chatcommand("bot_mine", {
    params = "[x y z]",
    description = "Dig a block with the wielded tool",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            minetest.chat_send_player(name, "BOT_MINE {\"ok\":false,\"status\":\"no_player\"}")
            return false, "player not found"
        end
        local target = parse_pos_params(param)
        if not target then
            target = front_pos(player)
        end
        local pos = player:get_pos()
        if vector.distance(pos, target) > 6 then
            minetest.chat_send_player(name, "BOT_MINE {\"ok\":false,\"status\":\"out_of_range\"}")
            return false, "out of range"
        end
        local node = minetest.get_node_or_nil(target)
        if not node or node.name == "air" or node.name == "ignore" then
            minetest.chat_send_player(name, "BOT_MINE {\"ok\":false,\"status\":\"no_block\"}")
            return false, "no block"
        end
        local def = minetest.registered_nodes[node.name]
        if not def or def.diggable == false then
            minetest.chat_send_player(name, "BOT_MINE {\"ok\":false,\"status\":\"not_diggable\"}")
            return false, "not diggable"
        end
        minetest.node_dig(target, node, player)
        minetest.chat_send_player(name, "BOT_MINE {\"ok\":true,\"status\":\"mined\"}")
        return true, "mined"
    end,
})

-- Retained for manual and older external clients. Current collection also uses
-- the native player dig protocol so tool timing and inventory updates are real.
minetest.register_chatcommand("bot_collect", {
    params = "<node_name> [count] [radius]",
    description = "Mine several nearby blocks of one exact node type",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            send_bot_json(name, "BOT_COLLECT", { ok = false, status = "no_player" })
            return false, "player not found"
        end
        local args = split_words(param)
        local node_name = args[1] or ""
        if node_name == "" or not minetest.registered_nodes[node_name] then
            send_bot_json(name, "BOT_COLLECT", {
                ok = false,
                status = "unknown_node",
                node = node_name,
            })
            return false, "unknown node"
        end
        local count = math.floor(math.max(1, math.min(tonumber(args[2]) or 1, 8)))
        local radius = math.floor(math.max(1, math.min(tonumber(args[3]) or 5, 6)))
        local pos = player:get_pos()
        local center = vector.round(pos)
        local rvec = { x = radius, y = radius, z = radius }
        local candidates = minetest.find_nodes_in_area(
            vector.subtract(center, rvec),
            vector.add(center, rvec),
            { node_name }
        )
        table.sort(candidates, function(a, b)
            return vector.distance(pos, a) < vector.distance(pos, b)
        end)
        local mined = 0
        for _, target in ipairs(candidates) do
            if mined >= count then
                break
            end
            if vector.distance(pos, target) <= 6 and not minetest.is_protected(target, name) then
                local node = minetest.get_node_or_nil(target)
                local def = node and minetest.registered_nodes[node.name] or nil
                if node and node.name == node_name and def and def.diggable ~= false then
                    minetest.node_dig(target, node, player)
                    local after = minetest.get_node_or_nil(target)
                    if after and after.name ~= node_name then
                        mined = mined + 1
                    end
                end
            end
        end
        local ok = mined > 0
        send_bot_json(name, "BOT_COLLECT", {
            ok = ok,
            status = ok and "collected" or "no_reachable_nodes",
            node = node_name,
            mined = mined,
            requested = count,
        })
        return ok, ok and ("mined " .. mined) or "no reachable nodes"
    end,
})

minetest.register_chatcommand("bot_place", {
    params = "[x y z]",
    description = "Place a block with the wielded item",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_player\"}")
            return false, "player not found"
        end
        local target = parse_pos_params(param)
        if not target then
            target = front_pos(player)
        end
        local pos = player:get_pos()
        if vector.distance(pos, target) > 6 then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"out_of_range\"}")
            return false, "out of range"
        end
        local itemstack = player:get_wielded_item()
        if itemstack:is_empty() then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_item\"}")
            return false, "no item"
        end
        local itemname = itemstack:get_name()
        local nodedef = minetest.registered_nodes[itemname]
        if not nodedef then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_item\"}")
            return false, "item not placeable"
        end
        if minetest.is_protected(target, name) then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_space\"}")
            return false, "target protected"
        end
        local existing = minetest.get_node_or_nil(target)
        if not existing then
            minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_space\"}")
            return false, "invalid target"
        end
        if existing.name ~= "air" and existing.name ~= "ignore" then
            local existing_def = minetest.registered_nodes[existing.name]
            if not existing_def or not existing_def.buildable_to then
                minetest.chat_send_player(name, "BOT_PLACE {\"ok\":false,\"status\":\"no_space\"}")
                return false, "target occupied"
            end
        end
        minetest.set_node(target, { name = itemname })
        itemstack:take_item(1)
        player:set_wielded_item(itemstack)
        minetest.chat_send_player(name, "BOT_PLACE {\"ok\":true,\"status\":\"placed\"}")
        return true, "placed"
    end,
})

minetest.register_chatcommand("bot_wield", {
    params = "<item>",
    description = "Wield an item from inventory",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "no_player" })
            return false, "player not found"
        end
        local item_name = (param or ""):gsub("^%s+", ""):gsub("%s+$", "")
        if item_name == "" then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "missing_item" })
            return false, "missing item"
        end
        local inv = player:get_inventory()
        if not inv then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "no_inventory" })
            return false, "no inventory"
        end
        local idx = tonumber(item_name)
        if idx then
            local list = inv:get_list("main") or {}
            if idx < 1 or idx > #list then
                send_bot_json(name, "BOT_WIELD", { ok = false, status = "invalid_slot" })
                return false, "invalid slot"
            end
            local stack = list[idx]
            if not stack or stack:is_empty() then
                send_bot_json(name, "BOT_WIELD", { ok = false, status = "empty_slot" })
                return false, "empty slot"
            end
            local ok, err = wield_inventory_slot(player, inv, idx, stack)
            if not ok then
                send_bot_json(name, "BOT_WIELD", { ok = false, status = "wield_failed", error = err })
                return false, err
            end
            send_bot_json(name, "BOT_WIELD", { ok = true, status = "wielded", item = stack:get_name() })
            return true, "wielded"
        end
        local slot, stack = find_inventory_item(inv, item_name)
        if not slot or not stack then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "not_found", item = item_name })
            return false, "item not found"
        end
        local ok, err = wield_inventory_slot(player, inv, slot, stack)
        if not ok then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "wield_failed", error = err })
            return false, err
        end
        send_bot_json(name, "BOT_WIELD", { ok = true, status = "wielded", item = item_name })
        return true, "wielded"
    end,
})

minetest.register_chatcommand("bot_drop", {
    params = "[item] [count]",
    description = "Drop an item from inventory or wielded",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            send_bot_json(name, "BOT_DROP", { ok = false, status = "no_player" })
            return false, "player not found"
        end
        local args = split_words(param)
        local item_name = args[1] or ""
        local count = tonumber(args[2]) or 1
        if count < 1 then
            count = 1
        end
        local drop_stack = nil
        local dropped_item = ""
        if item_name == "" then
            local wield = player:get_wielded_item()
            if wield:is_empty() then
                send_bot_json(name, "BOT_DROP", { ok = false, status = "no_item" })
                return false, "no item"
            end
            dropped_item = wield:get_name()
            drop_stack = wield:take_item(count)
            player:set_wielded_item(wield)
        else
            local inv = player:get_inventory()
            if not inv then
                send_bot_json(name, "BOT_DROP", { ok = false, status = "no_inventory" })
                return false, "no inventory"
            end
            local slot, stack = find_inventory_item(inv, item_name)
            if not slot or not stack then
                send_bot_json(name, "BOT_DROP", { ok = false, status = "not_found", item = item_name })
                return false, "item not found"
            end
            dropped_item = stack:get_name()
            drop_stack = stack:take_item(count)
            inv:set_stack("main", slot, stack)
        end
        if not drop_stack or drop_stack:is_empty() then
            send_bot_json(name, "BOT_DROP", { ok = false, status = "no_item" })
            return false, "no item"
        end
        minetest.item_drop(drop_stack, player, player:get_pos())
        send_bot_json(name, "BOT_DROP", {
            ok = true,
            status = "dropped",
            item = dropped_item,
            count = drop_stack:get_count(),
        })
        return true, "dropped"
    end,
})

minetest.register_chatcommand("bot_use", {
    params = "[item]",
    description = "Use the wielded item or specified item",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            send_bot_json(name, "BOT_USE", { ok = false, status = "no_player" })
            return false, "player not found"
        end
        local item_name = (param or ""):gsub("^%s+", ""):gsub("%s+$", "")
        if item_name ~= "" then
            local inv = player:get_inventory()
            if not inv then
                send_bot_json(name, "BOT_USE", { ok = false, status = "no_inventory" })
                return false, "no inventory"
            end
            local slot, stack = find_inventory_item(inv, item_name)
            if not slot or not stack then
                send_bot_json(name, "BOT_USE", { ok = false, status = "not_found", item = item_name })
                return false, "item not found"
            end
            local ok, err = wield_inventory_slot(player, inv, slot, stack)
            if not ok then
                send_bot_json(name, "BOT_USE", { ok = false, status = "wield_failed", error = err })
                return false, err
            end
        end
        local wield = player:get_wielded_item()
        if wield:is_empty() then
            send_bot_json(name, "BOT_USE", { ok = false, status = "no_item" })
            return false, "no item"
        end
        local used_name = wield:get_name()
        local def = minetest.registered_items[used_name]
        if not def then
            send_bot_json(name, "BOT_USE", { ok = false, status = "unknown_item", item = used_name })
            return false, "unknown item"
        end
        local pointed = { type = "nothing" }
        local new_stack = nil
        local used = false
        if def.on_use then
            new_stack = def.on_use(wield, player, pointed)
            used = true
        elseif def.on_secondary_use then
            new_stack = def.on_secondary_use(wield, player, pointed)
            used = true
        elseif def.on_place then
            new_stack = def.on_place(wield, player, pointed)
            used = true
        end
        if not used then
            send_bot_json(name, "BOT_USE", { ok = false, status = "not_usable", item = used_name })
            return false, "item not usable"
        end
        if new_stack then
            player:set_wielded_item(new_stack)
        else
            player:set_wielded_item(wield)
        end
        send_bot_json(name, "BOT_USE", { ok = true, status = "used", item = used_name })
        return true, "used"
    end,
})
