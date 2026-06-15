pub mod v1;

contract! {
    pub enum Detections {
        "1" => V1(v1::Detections),
    }
}

contract! {
    pub enum PerceptionState {
        "1" => V1(v1::PerceptionState),
    }
}
