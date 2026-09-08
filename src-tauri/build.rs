fn main() {
  tauri_build::build();
  // Cargo examples do not inherit the desktop executable's resource manifest.
  // Wry imports TaskDialogIndirect, which requires common-controls v6.
  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
    println!("cargo:rustc-link-arg-examples=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
  }
}
