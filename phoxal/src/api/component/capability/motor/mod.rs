pub mod v1;

contract! {
    pub enum Command {
        V1(v1::Command),
    }
}
