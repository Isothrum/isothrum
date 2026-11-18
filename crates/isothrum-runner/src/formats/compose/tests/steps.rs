//! The steps a start says: its images, then its containers made, started and waited on, from
//! what its pull and its `up` print.

use super::*;
use crate::events::EventKind;
use crate::steps::Steps;
use crate::steps::said::Said;

const PULLED: &str = " web Pulling \n db Pulling \n36b0bafd7f97 Downloading [==>   ] 1.2MB/3MB\n db Pulled \n web Pulled \n";
const STARTED: &str = " Network stack_default  Creating
 Network stack_default  Created
 Container stack-db-1  Creating
 Container stack-web-1  Creating
 Container stack-db-1  Created
 Container stack-web-1  Created
 Container stack-db-1  Starting
 Container stack-web-1  Starting
 Container stack-db-1  Started
 Container stack-web-1  Started
 Container stack-db-1  Waiting
 Container stack-web-1  Waiting
 Container stack-db-1  Healthy
 Container stack-web-1  Healthy
";

#[tokio::test]
async fn a_start_says_its_images_then_its_containers_made_started_and_healthy() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect(PULL, Reply::ok(PULLED))
        .expect(UP, Reply::ok(STARTED))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    let said = Said::default();
    let say = |kind: EventKind| said.keep(kind);
    let cx = ToolCx {
        steps: Steps::new(&say),
        ..cx(&project, &run, &fake)
    };
    Compose.up(&cx).await.expect("up");
    let ended: Vec<String> = said.lines().into_iter().filter(|l| !l.contains(" started")).collect();
    assert_eq!(ended, ["images done 2/2", "create done 2/2", "start done 2/2", "health done 2/2"]);
}

#[tokio::test]
async fn a_start_that_fails_making_its_containers_fails_that_step() {
    let (_dir, project) = folder();
    let run = run();
    let refused = " Container stack-db-1  Creating\nError response from daemon: no space left on device\n";
    let fake = FakeProcesses::script()
        .allow(PULL, Reply::ok(""))
        .expect(UP, Reply::fail(1, refused))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect(ENGINE_ASKED, Reply::ok("29.9.3"))
        .build();
    let said = Said::default();
    let say = |kind: EventKind| said.keep(kind);
    let cx = ToolCx {
        steps: Steps::new(&say),
        ..cx(&project, &run, &fake)
    };
    Compose.up(&cx).await.expect_err("no space");
    // What the operation does with what is still open, once the driver failed.
    cx.steps.end(false);
    assert_eq!(
        said.lines(),
        ["images started", "images done", "create started", "create started 0/1", "create failed 0/1"]
    );
}
