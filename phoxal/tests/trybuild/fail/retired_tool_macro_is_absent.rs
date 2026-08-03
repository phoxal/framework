// The tool concept is gone (#978): the supervisor absorbed the resident tools
// and the rest became CLI companions, so there is no authoring kind left to
// attach.
#[phoxal::tool(id = "retired")]
struct RetiredTool;

fn main() {}
