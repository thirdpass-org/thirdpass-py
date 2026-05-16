use thirdpass_core::extension::FromLib;
use thirdpass_py_lib;

fn main() {
    let mut extension = thirdpass_py_lib::PyExtension::new();
    thirdpass_core::extension::run_command(&mut extension).unwrap();
}
