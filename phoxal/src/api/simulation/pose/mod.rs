pub mod v1;

contract! {
    pub enum Pose {
        V1(v1::Pose),
    }
}
