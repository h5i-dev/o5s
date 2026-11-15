#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (dir, repo) = pico2peta_server::fuzzing::temp_repo();
    pico2peta_server::fuzzing::git_push(repo, dir.path(), data);
});
