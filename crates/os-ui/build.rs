// Stile fluent su tutte le piattaforme: lo stesso aspetto su Linux e su Windows (doc 12.15).
fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
