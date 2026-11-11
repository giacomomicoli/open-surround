// Stile fluent su tutte le piattaforme: lo stesso aspetto su Linux e su Windows (doc 12.15).
// Nelle build di debug (quelle dei test) anche le informazioni che servono al backend di prova
// di Slint per trovare e cliccare gli elementi (test `clic` in main.rs).
fn main() {
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into()).with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
