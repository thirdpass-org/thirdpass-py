use thirdpass_lib::extension::FromLib;
use thirdpass_py_lib;

fn main() {
    let mut extension = thirdpass_py_lib::PyExtension::new();
    thirdpass_lib::extension::commands::run(&mut extension).unwrap();
}
