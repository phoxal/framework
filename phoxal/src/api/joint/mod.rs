pub mod v1;

contract! {
    pub enum JointState {
        V1(v1::JointState),
    }
}
