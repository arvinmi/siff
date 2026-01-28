pub mod file_tree;

use ratatui::{Frame, widgets::ListState};

use crate::types::{AppState, Focus};

/// Main UI state that holds all component states.
#[derive(Default)]
pub struct UIState {
  pub file_tree_list_state: ListState,
}

/// Renders the complete UI.
/// Main entry point for all UI rendering.
pub fn render_app(terminal_frame: &mut Frame, app_state: &AppState, ui_state: &mut UIState) {
  // use the original integrated layout that shows config and file tree
  file_tree::render_file_tree_with_options(
    terminal_frame,
    terminal_frame.area(),
    app_state,
    &mut ui_state.file_tree_list_state,
    app_state.token_count,
    &app_state.status_message,
  );
}

/// Handles keyboard input for the entire app.
/// Routes input to the appropriate component based on current focus.
pub fn handle_input(app_state: &mut AppState, _ui_state: &mut UIState, key: crossterm::event::KeyEvent) -> bool {
  // global shortcuts that work regardless of focus (r)
  if let crossterm::event::KeyCode::Char('r') = key.code {
    // run repomixm, handled by the main app loop
    return false;
  }

  // route input based on current focus (file tree)
  match app_state.focus {
    Focus::FileTree => file_tree::handle_file_tree_input(app_state, key),
  }
}

/// Updates the UI state after app state changes.
/// UI components stay in sync with the app.
pub fn update_ui_state(app_state: &AppState, ui_state: &mut UIState) {
  // update file tree selection to match app state (if not empty)
  if !app_state.visible_paths.is_empty() {
    let selected = app_state.selected_index.min(app_state.visible_paths.len() - 1);
    ui_state.file_tree_list_state.select(Some(selected));
  } else {
    ui_state.file_tree_list_state.select(None);
  }
}
