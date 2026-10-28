//! Tiny example binary listing Windows audio render endpoints.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use capture::wasapi::{ComMtaGuard, list_render_endpoints};

    println!("Initializing COM MTA...");
    let _com = ComMtaGuard::new()?;

    println!("Querying audio render endpoints...");
    let endpoints = list_render_endpoints()?;
    println!("Found {} audio render endpoint(s):", endpoints.len());
    for (i, ep) in endpoints.iter().enumerate() {
        println!(
            "  [{}] {} (default: {}) [ID: {}]",
            i + 1,
            ep.friendly_name,
            ep.is_default,
            ep.id
        );
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    println!("This example is intended for Windows targets.");
}
