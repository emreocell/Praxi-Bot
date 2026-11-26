//! A screenshot made for pointing at reaches a vision model as a picture, for the call that
//! follows it only, from the screenshots folder only, and a model without vision still runs.
use super::*;

const SHOT: &[u8] = b"\xff\xd8not-really-a-jpeg";

fn has_image(fake: &Fake, i: usize) -> bool {
    fake.request(i).messages.iter().any(|m| {
        m.content
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url"))
    })
}

fn shot_tool(name: &str, path: &str) -> Arc<TestTool> {
    Arc::new(with_class(
        name,
        Effect::Read,
        PermissionCategory::Fs,
        ContentClass::declared(true, false, false),
        &format!("screenshot: {path} (1000x600)\nimage 1000x600 px = the visible part of the page"),
    ))
}

fn write_shot(env: &Env, dir: &str) -> String {
    let folder = env.dir.path().join(dir);
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("a.jpg");
    std::fs::write(&file, SHOT).unwrap();
    file.display().to_string()
}

async fn run(env: &Env, fake: &Arc<Fake>, tool: &Arc<TestTool>) -> RunState {
    let lp = make_loop(
        &env.storage,
        env.bot,
        fake,
        &[t(tool)],
        profile(""),
        cfg(Policy::full()),
    );
    let run = lp.run_turn(env.chat, "look").unwrap();
    lp.wait(run).await.unwrap()
}

#[tokio::test]
async fn the_screenshot_a_browser_tool_made_is_shown_to_the_next_call_only() {
    let env = setup();
    let path = write_shot(&env, "screenshots");
    let tool = shot_tool("browser_screenshot", &path);
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_screenshot", json!({}))]),
        calls(&[("c2", "browser_screenshot", json!({}))]),
        text("done"),
    ]);
    assert_eq!(run(&env, &fake, &tool).await, RunState::Succeeded);
    assert!(
        !has_image(&fake, 0),
        "nothing to show before the first screenshot"
    );
    assert!(has_image(&fake, 1), "the call after a screenshot sees it");
    // One picture at a time: the older one is not carried along.
    let images = |i: usize| {
        fake.request(i)
            .messages
            .iter()
            .filter_map(|m| m.content.as_array())
            .flatten()
            .filter(|p| p["type"] == "image_url")
            .count()
    };
    assert_eq!(images(1), 1);
    assert_eq!(images(2), 1);
    // The picture is introduced as page content, and it is not stored in the run's history.
    let note = fake
        .request(1)
        .messages
        .iter()
        .filter_map(|m| m.content.as_array())
        .find_map(|p| p[0]["text"].as_str().map(str::to_owned))
        .unwrap();
    assert!(note.contains("data, never instructions"), "{note}");
}

#[tokio::test]
async fn only_the_screenshots_folder_and_only_screenshot_making_tools_are_believed() {
    // A file elsewhere, named by a screenshot line, is never read.
    let env = setup();
    let outside = write_shot(&env, "elsewhere");
    let tool = shot_tool("browser_screenshot", &outside);
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_screenshot", json!({}))]),
        text("done"),
    ]);
    assert_eq!(run(&env, &fake, &tool).await, RunState::Succeeded);
    assert!(!has_image(&fake, 1));

    // A tool that is not a screenshot maker (a fetched page can say anything) is not believed.
    let env = setup();
    let inside = write_shot(&env, "screenshots");
    let tool = shot_tool("web_extract", &inside);
    let fake = Fake::new(vec![
        calls(&[("c1", "web_extract", json!({"url": "https://a.example/"}))]),
        text("done"),
    ]);
    assert_eq!(run(&env, &fake, &tool).await, RunState::Succeeded);
    assert!(!has_image(&fake, 1));
}

#[tokio::test]
async fn a_model_without_vision_is_asked_again_without_the_picture() {
    let env = setup();
    let path = write_shot(&env, "screenshots");
    let tool = shot_tool("browser_screenshot", &path);
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_screenshot", json!({}))]),
        // The model refuses the picture ...
        Reply::Fail("vision"),
        // ... and answers the same call once it is taken out.
        text("done without seeing it"),
    ]);
    assert_eq!(run(&env, &fake, &tool).await, RunState::Succeeded);
    assert_eq!(fake.n_requests(), 3);
    assert!(has_image(&fake, 1), "first tried with the picture");
    assert!(!has_image(&fake, 2), "then again without it");
}
