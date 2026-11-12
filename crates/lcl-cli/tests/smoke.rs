mod common;

use common::Env;

#[test]
fn init_create_show_round_trips_title() {
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();

    let created = env.lcl(&["issue", "create", "--title", "x", "--json"]).assert_ok().json();
    assert_eq!(created["title"], "x", "create: {created:#}");

    let shown = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json();
    assert_eq!(shown["issue"]["title"], "x", "show: {shown:#}");
    assert_eq!(shown["issue"]["id"], created["id"], "show 1 returned a different issue");
}
