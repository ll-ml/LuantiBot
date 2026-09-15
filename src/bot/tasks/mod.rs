//! Native task state machines driven by server acknowledgements.

mod crafting;
mod inventory;
mod mining;

pub(super) use crafting::{
    advance_native_craft, cancel_native_craft, continue_native_craft, fail_native_craft,
    handle_native_craft_prepare, handle_native_craft_receipt, NativeCraftTask,
};
pub(super) use inventory::{
    advance_native_inventory_transfer, cancel_native_inventory_transfer,
    continue_native_inventory_transfer, fail_native_inventory_transfer,
    handle_native_inventory_prepare, handle_native_inventory_receipt,
    NativeInventoryTransferTask,
};
pub(super) use mining::{
    advance_native_mining, cancel_native_mining, continue_native_mining, face_native_target,
    fail_current_native_target, handle_native_mine_prepare, handle_native_mine_verify,
    native_collect_targets,
    NativeMiningOrigin, NativeMiningTask,
};
pub(crate) use inventory::{API_INVENTORY_TRANSFER_TIMEOUT, InventoryTransferOperation};

#[cfg(test)]
pub(in crate::bot) use inventory::{
    NativeInventoryTransferPhase, NATIVE_INVENTORY_TASK_TIMEOUT,
};
