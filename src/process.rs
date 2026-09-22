//! Process groups keep cancelled read-only helpers from outliving the palette.
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub(crate) fn isolate(command: &mut Command) {
    command.process_group(0);
}

pub(crate) fn terminate(child: &mut Child) {
    unsafe extern "C" {
        fn killpg(pgid: i32, signal: i32) -> i32;
    }
    // Only children created with isolate() are passed here. Their PID is their
    // process-group ID, so descendants holding inherited pipes die as well.
    unsafe {
        killpg(child.id() as i32, 9);
    }
    let _ = child.kill();
}
