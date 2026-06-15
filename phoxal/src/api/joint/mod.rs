pub mod v1;

contract! {
    pub enum JointState {
        "1" => V1(v1::JointState),
    }
}
