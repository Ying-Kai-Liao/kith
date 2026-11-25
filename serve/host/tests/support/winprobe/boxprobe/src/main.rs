//! W20 guest box probe: builds an AppContainer box (custom capability SIDs, a job, one CreateProcess carrying
//! SECURITY_CAPABILITIES and JOB_LIST) and holds the checks that run inside it. CI-only (windows-probe.yml `guest`).
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    win::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("boxprobe: Windows only");
    std::process::exit(2);
}
