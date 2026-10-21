use std::{env, path::Path, thread, time::Duration};

const GREETER_STOP_FILE: &str = ".alphin-e2e-stop-greeter";

fn main() {
    let greeter = env::args()
        .next()
        .as_deref()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "alphin-e2e-greeter");
    let stop_file =
        env::var_os("XDG_RUNTIME_DIR").map(|runtime| Path::new(&runtime).join(GREETER_STOP_FILE));
    loop {
        if greeter && stop_file.as_ref().is_some_and(|path| path.exists()) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}
