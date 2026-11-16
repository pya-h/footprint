fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("base directory is required")?,
    );
    std::fs::create_dir_all(&base)?;
    let scenario = fp_fixtures::fake_zoom(&base)?;
    println!("home={}", scenario.home.display());
    println!("root={}", scenario.root.display());
    println!("since=@{}", fp_fixtures::local(0, 0, 0));
    println!("until=@{}", fp_fixtures::local(23, 59, 59));
    std::mem::forget(scenario.browser_lock);
    Ok(())
}
