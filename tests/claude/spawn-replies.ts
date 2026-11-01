// Spawn replies printed by mux-agent (JSON and -Text), success and the tagged refusal.
// Pinned by src/cli.rs `spawn_replies_fixture_matches`, which runs a real spawn over the fakes and fails on any drift.
// The random id and sessionId are fixed to 0123456789ab and 00000000-0000-4000-8000-000000000000.
export const SPAWN_JSON = "{\"ok\":true,\"verb\":\"spawn\",\"name\":\"claude-1\",\"id\":\"0123456789ab\",\"pane\":\"%9\",\"window\":\"@5\",\"client\":\"claude\",\"sessionId\":\"00000000-0000-4000-8000-000000000000\",\"context\":\"brief\",\"reason\":\"human\",\"placement\":\"tab\",\"placementSource\":\"default\"}"
export const TAGGED_JSON = "{\"ok\":false,\"verb\":\"spawn\",\"code\":2,\"check\":\"tagged\",\"message\":\"an agent pane cannot spawn agents\"}"
export const SPAWN_TEXT = "name: claude-1\nid: 0123456789ab\npane: %9\nwindow: @5\nclient: claude\nsessionId: 00000000-0000-4000-8000-000000000000\ncontext: brief\nreason: human\nplacement: tab\nplacementSource: default"
export const TAGGED_TEXT = "code: 2\ncheck: tagged\nmessage: an agent pane cannot spawn agents"
