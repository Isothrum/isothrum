//! An update runs the tool's own update once.

use super::pins::compose_lab;
use super::*;

#[tokio::test]
async fn an_update_of_a_compose_run_with_an_attached_service_runs_one_up() {
    let (_root, project, runner, fake) = compose_lab();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    run.attach("attackbox", &RunOptions::new(), &Recorder::new()).await.expect("attached");
    let before = ups(&fake);
    run.update(&RunOptions::new(), &Recorder::new()).await.expect("updated");
    assert_eq!(ups(&fake) - before, 1, "one up, the attached service no orphan to remove again");
}
