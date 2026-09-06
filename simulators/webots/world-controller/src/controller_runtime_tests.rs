use super::*;

#[test]
fn basic_time_step_must_be_a_positive_integer() {
    assert_eq!(exact_step_ms(12.0).expect("valid step"), 12);
    for invalid in [0.0, -1.0, 12.5, f64::NAN] {
        assert!(exact_step_ms(invalid).is_err());
    }
}

#[test]
fn progress_time_is_quantized_to_nanoseconds() {
    assert_eq!(observed_elapsed_ns(0.012).expect("valid time"), 12_000_000);
    assert!(observed_elapsed_ns(-1.0).is_err());
}

#[test]
fn every_post_initialization_failure_forces_native_convergence() {
    let mut quit = false;
    let result = converge_on_error::<()>(Err(anyhow::anyhow!("host bootstrap failed")), || {
        quit = true;
    });
    assert!(result.is_err());
    assert!(quit);

    let mut quit = false;
    converge_on_error(Ok(()), || quit = true).expect("normal controller exit");
    assert!(!quit);
}
