const REQUIRE_NATIVE_ENV: &str = "PHOXAL_REQUIRE_NATIVE_WEBOTS";
const REQUIRE_NATIVE_CFG: &str = "phoxal_require_native_webots";

fn main() {
    println!("cargo:rerun-if-env-changed={REQUIRE_NATIVE_ENV}");
    println!("cargo:rustc-check-cfg=cfg({REQUIRE_NATIVE_CFG})");

    if std::env::var_os(REQUIRE_NATIVE_ENV).is_some() {
        println!("cargo:rustc-cfg={REQUIRE_NATIVE_CFG}");
    }
}
