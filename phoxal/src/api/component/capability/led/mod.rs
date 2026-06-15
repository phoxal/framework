pub mod v1;

contract! {
    pub enum Command {
        "1" => V1(v1::Command),
    }
}
