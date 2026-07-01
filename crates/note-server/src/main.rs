mod notes_api;

fn main() {
    // Task 23 wires notes_api::notes_router() into the running Axum server. Reference it here so
    // the module (and its handlers) compile as live code rather than dead_code in this task.
    let _router = notes_api::notes_router();
    println!("note-server stub");
}
