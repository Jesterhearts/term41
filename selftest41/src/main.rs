mod app;
mod capabilities;
mod demo;
mod terminal_io;
mod ui;

use std::io;
use std::time::Duration;

use app::App;
use app::AppCommand;

fn main() -> io::Result<()> {
    let mut session = terminal_io::TerminalSession::enter()?;
    let capabilities = session.probe_capabilities()?;
    let demos = demo::catalog();
    let mut app = App::new(demos, capabilities);

    while !app.should_quit() {
        session.terminal_mut().draw(|frame| ui::draw(frame, &app))?;
        let Some(event) = session.poll_event(Duration::from_millis(100))? else {
            continue;
        };
        let size = session.terminal_mut().size()?;
        match app.handle_event(event, size.into()) {
            AppCommand::None | AppCommand::Quit => {}
            AppCommand::RunSelected => {
                let demo_id = app.selected_demo().id;
                let caps = app.capabilities().clone();
                session.run_demo(demo_id, &caps)?;
            }
            AppCommand::Reprobe => {
                let capabilities = session.probe_capabilities()?;
                app.set_capabilities(capabilities);
            }
        }
    }

    Ok(())
}
