pub mod v1;

contract! {
    pub enum Collision {
        "1" => V1(v1::Collision),
    }
}
