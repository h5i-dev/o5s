#![no_main]
//! DSSE envelopes and in-toto statements (attacker-supplied JSON).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(env) = pico2peta_attest::Envelope::from_json(data) {
        let _ = env.statement();
        let _ = env.verified_by(&[]);
        let _ = pico2peta_attest::Envelope::from_json(&env.to_json());
    }
    let _ = pico2peta_attest::Statement::from_json(data);
});
