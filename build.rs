//! Emits the linker flags that turn the cdylib into a Windows kernel driver.

fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()
}
