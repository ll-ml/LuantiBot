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
    local toolcaps = {
        full_punch_interval = 0.1,
        max_drop_level = 0,
        damage_groups = { fleshy = 2 },
    }
    target:punch(attacker, 1.0, toolcaps, dir)
    return true, "punched"
end

minetest.register_chatcommand("bot_attack", {
    params = "<player>",
    description = "Server-side punch a player as the caller",
    privs = { interact = true },
    func = function(name, param)
        local attacker = get_player(name)
        if not attacker then
            return false, "attacker not found"
        end
        local target_name = param:gsub("^%s+", ""):gsub("%s+$", "")
        if target_name == "" then
            return false, "missing target name"
        end
        local target = get_player(target_name)
        if not target then
            return false, "target not found"
        end
        local ok, msg = punch_target(attacker, target)
        if ok then
            minetest.log("action", "[llm_bot] " .. name .. " punched " .. target_name)
        end
        return ok, msg
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

local function is_hostile_entity(ent_def)
    if not ent_def then
        return false
    end
    if ent_def.type == "monster" or ent_def.type == "hostile" then
        return true
    end
    if ent_def.groups and (ent_def.groups.monster or ent_def.groups.hostile) then
        return true
    end
    if ent_def.attack_type or ent_def.attack_players or ent_def.attack_player or ent_def.attack then
        return true
    end
    return false
end

local function parse_pos_params(param)
    local nums = {}
    for value in string.gmatch(param or "", "[^%s]+") do
        local n = tonumber(value)
        if not n then
            return nil
        end
        nums[#nums + 1] = n
        if #nums >= 3 then
            break
        end
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

local function send_bot_json(name, tag, payload)
    if minetest.write_json then
        minetest.chat_send_player(name, tag .. " " .. minetest.write_json(payload))
    else
        minetest.chat_send_player(name, tag .. " " .. minetest.serialize(payload))
    end
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

local function build_observe(player, radius)
    local pos = player:get_pos()
    local node_pos = vector.round(pos)
    local facing = get_cardinal_facing(player)
    local obstacles = get_obstacles(node_pos, facing)

    local nodes = {}
    local node_limit = 200
    local node_total = 0
    local scan_radius = math.min(radius, 1)
    for y = -scan_radius, scan_radius do
        for x = -scan_radius, scan_radius do
            for z = -scan_radius, scan_radius do
                local p = {
                    x = node_pos.x + x,
                    y = node_pos.y + y,
                    z = node_pos.z + z,
                }
                local node = minetest.get_node_or_nil(p)
                if node and node.name and node.name ~= "air" and node.name ~= "ignore" then
                    local def = minetest.registered_nodes[node.name]
                    if def and def.walkable and def.diggable ~= false then
                        node_total = node_total + 1
                        if #nodes < node_limit then
                            nodes[#nodes + 1] = {
                                pos = { p.x, p.y, p.z },
                                name = node.name,
                            }
                        end
                    end
                end
            end
        end
    end

    local hostiles = {}
    local hostile_limit = 8
    for _, obj in ipairs(minetest.get_objects_inside_radius(pos, radius + 2)) do
        if not obj:is_player() then
            local ent = obj:get_luaentity()
            if ent and ent.name then
                local ent_def = minetest.registered_entities[ent.name]
                if is_hostile_entity(ent_def) and #hostiles < hostile_limit then
                    local tpos = obj:get_pos()
                    hostiles[#hostiles + 1] = {
                        type = ent.name,
                        dx = math.floor(tpos.x - node_pos.x + 0.5),
                        dy = math.floor(tpos.y - node_pos.y + 0.5),
                        dz = math.floor(tpos.z - node_pos.z + 0.5),
                    }
                end
            end
        end
    end

    local items = {}
    local item_limit = 12
    local rvec = { x = radius, y = radius, z = radius }
    local minp = vector.subtract(node_pos, rvec)
    local maxp = vector.add(node_pos, rvec)
    local item_nodes = minetest.find_nodes_in_area(minp, maxp, {
        "group:tree",
        "group:wood",
        "group:flora",
        "group:plant",
        "group:leaves",
    })
    for _, p in ipairs(item_nodes) do
        if #items >= item_limit then
            break
        end
        local node = minetest.get_node(p)
        if node and node.name and node.name ~= "air" and node.name ~= "ignore" then
            items[#items + 1] = {
                type = node.name,
                dx = p.x - node_pos.x,
                dy = p.y - node_pos.y,
                dz = p.z - node_pos.z,
            }
        end
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
        inventory.wield = {
            name = wield:get_name(),
            count = wield:get_count(),
            wear = wield:get_wear(),
        }
        local main = inv:get_list("main") or {}
        local items = {}
        local item_limit = 30
        for _, stack in ipairs(main) do
            if stack and not stack:is_empty() then
                items[#items + 1] = {
                    name = stack:get_name(),
                    count = stack:get_count(),
                }
                if #items >= item_limit then
                    break
                end
            end
        end
        inventory.main = items
        inventory.main_truncated = #items >= item_limit
    end

    local data = {
        health = player:get_hp(),
        position = { node_pos.x, node_pos.y, node_pos.z },
        facing = facing,
        nodes = nodes,
        node_total = node_total,
        node_limit = node_limit,
        node_truncated = node_total > node_limit,
        inventory = inventory,
        hostiles = hostiles,
        items = items,
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
        radius = math.max(1, math.min(radius, 8))
        local json = build_observe(player, radius)
        minetest.chat_send_player(name, "BOT_OBSERVE " .. json)
        return true, "ok"
    end,
})

minetest.register_chatcommand("bot_attack_mobs", {
    params = "[radius]",
    description = "Punch nearest mob within radius",
    privs = { interact = true },
    func = function(name, param)
        local attacker = get_player(name)
        if not attacker then
            return false, "attacker not found"
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
                    local d = vector.distance(pos, obj:get_pos())
                    if d < nearest_dist then
                        nearest = obj
                        nearest_dist = d
                    end
                end
            end
        end
        if not nearest then
            return false, "no mobs in range"
        end
        local ok, msg = punch_target(attacker, nearest)
        if ok then
            minetest.log("action", "[llm_bot] " .. name .. " punched mob")
        end
        return ok, msg
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
            return false, "player not found"
        end
        local args = param:split(" ")
        local target_name = args[1]
        if not target_name or target_name == "" then
            return false, "missing target"
        end
        local radius = tonumber(args[2]) or 20
        radius = math.max(1, math.min(radius, 50))
        local obj = find_target_object(target_name, radius, player:get_pos())
        if not obj then
            return false, "target not found"
        end
        local pos = obj:get_pos()
        player:set_pos({ x = pos.x, y = pos.y, z = pos.z })
        return true, "approached"
    end,
})

minetest.register_chatcommand("bot_interact", {
    params = "<name> [radius]",
    description = "Interact with a target player/entity",
    privs = { interact = true },
    func = function(name, param)
        local player = get_player(name)
        if not player then
            return false, "player not found"
        end
        local args = param:split(" ")
        local target_name = args[1]
        if not target_name or target_name == "" then
            return false, "missing target"
        end
        local radius = tonumber(args[2]) or 6
        radius = math.max(1, math.min(radius, 20))
        local obj = find_target_object(target_name, radius, player:get_pos())
        if not obj then
            return false, "target not found"
        end
        local ok, msg = punch_target(player, obj)
        return ok, msg
    end,
})

minetest.register_chatcommand("bot_fight", {
    params = "<name> [radius]",
    description = "Attack a target player/entity",
    privs = { interact = true },
    func = function(name, param)
        return minetest.registered_chatcommands.bot_interact.func(name, param)
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
            if type(player.set_wield_index) == "function" then
                player:set_wield_index(idx)
            elseif type(player.get_wield_index) == "function" then
                local current = player:get_wield_index()
                local current_stack = list[current]
                inv:set_stack("main", current, stack)
                inv:set_stack("main", idx, current_stack or ItemStack(""))
                player:set_wielded_item(stack)
            else
                inv:set_stack("main", idx, ItemStack(""))
                player:set_wielded_item(stack)
            end
            send_bot_json(name, "BOT_WIELD", { ok = true, status = "wielded", item = stack:get_name() })
            return true, "wielded"
        end
        local slot, stack = find_inventory_item(inv, item_name)
        if not slot or not stack then
            send_bot_json(name, "BOT_WIELD", { ok = false, status = "not_found", item = item_name })
            return false, "item not found"
        end
        if type(player.set_wield_index) == "function" then
            player:set_wield_index(slot)
        elseif type(player.get_wield_index) == "function" then
            local current = player:get_wield_index()
            local list = inv:get_list("main") or {}
            local current_stack = list[current]
            inv:set_stack("main", current, stack)
            inv:set_stack("main", slot, current_stack or ItemStack(""))
            player:set_wielded_item(stack)
        else
            inv:set_stack("main", slot, ItemStack(""))
            player:set_wielded_item(stack)
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
        local args = (param or ""):split(" ")
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
            player:set_wield_index(slot)
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
