mod labels_api;
mod notes_api;

fn main() {
    // Task 23 wires these routers into the running Axum server. Reference them here so the modules
    // (and their handlers) compile as live code rather than dead_code in this task.
    let _ = (notes_api::notes_router(), labels_api::labels_router());
    println!("note-server stub");
}
