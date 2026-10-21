use std::{thread, time::Duration};

fn main() {
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
