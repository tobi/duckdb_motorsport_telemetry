//! Generate an MTJ integration fixture with the pinned upstream writer.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: generate_recording SOURCE OUTPUT.telemetry".into());
    }
    let source = telemetry::open(&args[0])?;
    telemetry_format::write_telemetry(source.as_ref(), &args[1])?;
    Ok(())
}
