pub mod v1;

contract! {
    pub enum Pose {
        "1" => V1(v1::Pose),
    }
}
