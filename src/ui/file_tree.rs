use std::{collections::HashMap, path::PathBuf};

use ratatui::{
  Frame,
  layout::{Constraint, Direction, Layout, Rect},
  style::{Color, Style},
  text::{Line, Span},
  widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::types::{AppState, FileNode};

/// Renders the combined file tree and options component.
/// which displays the configuration at top and file tree below.
pub fn render_file_tree_with_options(
  terminal_frame: &mut Frame,
  terminal_frame_area: Rect,
  app_state: &AppState,
  file_tree_list_state: &mut ListState,
  token_count: usize,
  status_message: &str,
) {
  match app_state.repomix_options.backend {
    crate::types::Backend::Repomix => {
      // for repomix backend, show both config and file tree
      let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
          // config box
          Constraint::Length(3),
          // file tree box
          Constraint::Min(0),
        ])
        .split(terminal_frame_area);

      // render config section
      render_configuration_section(terminal_frame, chunks[0], app_state);

      // render file tree section with hints and status
      render_file_tree_section_with_hints(
        terminal_frame,
        chunks[1],
        app_state,
        file_tree_list_state,
        token_count,
        status_message,
      );
    }
    crate::types::Backend::Yek => {
      // for yek backend, show only file tree
      render_file_tree_section_with_hints(
        terminal_frame,
        terminal_frame_area,
        app_state,
        file_tree_list_state,
        token_count,
        status_message,
      );
    }
  }
}

/// Renders config section at the top.
fn render_configuration_section(frame: &mut Frame, area: Rect, app_state: &AppState) {
  // create options content (only for repomix)
  let options = &app_state.repomix_options;

  // create colored spans for options
  let compress_symbol = if options.compress { "●" } else { "○" };
  let compress_color = if options.compress { Color::Green } else { Color::Gray };

  let remove_comments_symbol = if options.remove_comments { "●" } else { "○" };
  let remove_comments_color = if options.remove_comments { Color::Green } else { Color::Gray };

  let file_tree_symbol = if options.file_tree { "●" } else { "○" };
  let file_tree_color = if options.file_tree { Color::Green } else { Color::Gray };

  let options_content = vec![
    Span::raw("Options: "),
    Span::styled(file_tree_symbol, Style::default().fg(file_tree_color)),
    Span::raw(" File Tree (t) │ "),
    Span::styled(compress_symbol, Style::default().fg(compress_color)),
    Span::raw(" Compress (c) │ "),
    Span::styled(remove_comments_symbol, Style::default().fg(remove_comments_color)),
    Span::raw(" Remove Comments (m) │ Format: "),
    Span::styled(
      options.output_format.display_name(),
      // will display format (XML, Markdown, Plain Text)
      Style::default().fg(Color::Green),
    ),
    Span::raw(" (f)"),
  ];

  // style config block
  let config_style = Style::default().fg(Color::Green);

  // create config block
  let config_block = Block::default().borders(Borders::ALL).title("Configuration").style(config_style);

  // create options paragraph
  let options_paragraph =
    Paragraph::new(Line::from(options_content)).block(config_block).style(Style::default().fg(Color::White));

  frame.render_widget(options_paragraph, area);
}

/// Renders file tree section with hints and status.
fn render_file_tree_section_with_hints(
  terminal_frame: &mut Frame,
  terminal_frame_area: Rect,
  app_state: &AppState,
  file_tree_list_state: &mut ListState,
  token_count: usize,
  status_message: &str,
) {
  // get selected count
  let selected_count = app_state.file_tree.values().filter(|node| node.is_selected && !node.is_directory).count();

  // get directory name from the root path
  let root_name = app_state.root_path.file_name().and_then(|name| name.to_str()).unwrap_or(".");

  // create title text based on backend
  let title_text = match app_state.repomix_options.backend {
    crate::types::Backend::Repomix => "File Tree (repomix)".to_string(),
    crate::types::Backend::Yek => "File Tree (yek)".to_string(),
  };

  // style title based on whether component has focus
  let title_style = Style::default().fg(Color::Green);

  // determine layout constraints based on status message
  let constraints = if !status_message.is_empty() {
    vec![
      Constraint::Length(1), // root directory and selected count
      Constraint::Min(0),    // file list
      Constraint::Length(3), // status message with padding
      Constraint::Length(2), // nav hints
    ]
  } else {
    vec![
      Constraint::Length(1), // root directory and selected count
      Constraint::Min(0),    // file list
      Constraint::Length(2), // nav hints
    ]
  };

  // create file tree block
  let file_tree_block = Block::default().borders(Borders::ALL).title(title_text).style(title_style);

  // get inner area before rendering the block
  let inner_area = file_tree_block.inner(terminal_frame_area);

  terminal_frame.render_widget(file_tree_block, terminal_frame_area);

  // render root directory name and selected count
  let info_text = format!("{}  •  Selected: {} items", root_name, selected_count);
  let info_paragraph = Paragraph::new(info_text).style(Style::default().fg(Color::Cyan));

  // create layout for inner content
  let inner_chunks = Layout::default().direction(Direction::Vertical).constraints(constraints).split(inner_area);

  // split the top line for info and token count
  let top_chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([
      Constraint::Min(0),     // directory info (left)
      Constraint::Length(15), // token count (right, reduced width)
    ])
    .split(inner_chunks[0]);

  terminal_frame.render_widget(info_paragraph, top_chunks[0]);

  // render token count in top-right with right alignment
  let token_text = format!("Tokens: {}", crate::token_counter::format_token_count(token_count));
  let token_paragraph =
    Paragraph::new(token_text).style(Style::default().fg(Color::Yellow)).alignment(ratatui::layout::Alignment::Right);
  terminal_frame.render_widget(token_paragraph, top_chunks[1]);

  // render the file list
  render_file_list_inner(terminal_frame, inner_chunks[1], app_state, file_tree_list_state);

  // render status message if present (above keyboard nav menu)
  let hints_index = if !status_message.is_empty() {
    // render status message with appropriate styling and padding
    let status_style = if status_message.contains("Success") || status_message.contains("Copied to clipboard") {
      Style::default().fg(Color::Green)
    } else if status_message.contains("Error") || status_message.contains("Failed") {
      Style::default().fg(Color::Red)
    } else if status_message.contains("Warning") {
      Style::default().fg(Color::Yellow)
    } else if status_message.contains("Running") || status_message.contains("Processing...") {
      Style::default().fg(Color::Cyan)
    } else {
      Style::default().fg(Color::White)
    };

    // create status content with padding
    let status_content = format!("\n{}", status_message);
    let status_paragraph = Paragraph::new(status_content).style(status_style);
    terminal_frame.render_widget(status_paragraph, inner_chunks[2]);

    3 // nav hints are at index 3 when status is present
  } else {
    2 // nav hints are at index 2 when no status
  };

  // render nav hints at bottom
  let hints_text = match app_state.repomix_options.backend {
    crate::types::Backend::Repomix => {
      "↑/↓ navigate • ←/→ collapse/expand dirs • Space select files • E expand all • C collapse all • A select all • U unselect all • r run • q quit"
    }
    crate::types::Backend::Yek => {
      "↑/↓ navigate • ←/→ collapse/expand dirs • Space select files • E expand all • C collapse all • A select all • U unselect all • r run • q quit"
    }
  };
  let hints_paragraph = Paragraph::new(hints_text).style(Style::default().fg(Color::Yellow));

  terminal_frame.render_widget(hints_paragraph, inner_chunks[hints_index]);
}

/// Creates a formatted list item for a single file or directory.
/// Handles indentation, icons, selection indicators, and token counts with color coding.
fn create_list_item(
  path: &PathBuf,
  file_tree: &HashMap<PathBuf, FileNode>,
  individual_token_counts: &HashMap<PathBuf, Option<usize>>,
  dir_descendants_map: &HashMap<PathBuf, bool>,
  is_highlighted: bool,
) -> ListItem<'static> {
  // get node from file tree
  let node = file_tree.get(path).unwrap();

  // adjust depth for rootless tree view (subtract 1 since we skip the root directory)
  let display_depth = node.depth.saturating_sub(1);

  // create indentation based on adjusted depth (2 spaces per level)
  let indent = "  ".repeat(display_depth);

  // choose appropriate icon and color based on file type and state
  let (icon, base_style) = if node.is_directory {
    let expansion_icon = if node.is_expanded { "[-]" } else { "[+]" };

    // determine directory color based on selection state
    let color = if is_highlighted {
      // when highlighted (blue background), use white text for contrast
      Color::White
    } else if node.is_selected {
      Color::Green // fully selected directory
    } else if display_depth > 0 && *dir_descendants_map.get(path).unwrap_or(&false) {
      Color::Yellow // directory with some selected children
    } else {
      Color::Cyan // unselected directory
    };

    (expansion_icon, Style::default().fg(color))
  } else {
    let selection_icon = if node.is_selected { "●" } else { "○" };
    let color = if is_highlighted {
      // when highlighted (blue background), use white text for contrast
      Color::White
    } else if node.is_selected {
      Color::Green
    } else {
      Color::White
    };
    (selection_icon, Style::default().fg(color))
  };

  // get token count for item
  // show for selected items or directories with selected descendants
  let should_show_tokens = if node.is_directory {
    // show token counts for all directories that are selected or have selected descendants
    node.is_selected || *dir_descendants_map.get(path).unwrap_or(&false)
  } else {
    node.is_selected
  };

  let token_count_opt = if should_show_tokens { individual_token_counts.get(path).and_then(|opt| *opt) } else { None };

  // create main display text
  let main_text = format!("{}{} {}", indent, icon, node.name);

  // create spans for list item
  let mut spans = vec![Span::styled(main_text, base_style)];

  // add token count display, only show actual counts
  if should_show_tokens && let Some(token_count) = token_count_opt {
    // show actual token count (even if 0)
    let token_color = if is_highlighted {
      // when highlighted, use light blue for token counts contrast
      Color::LightBlue
    } else {
      get_token_count_color(token_count)
    };
    let token_text = format!(" ({})", crate::token_counter::format_token_count(token_count));
    spans.push(Span::styled(token_text, Style::default().fg(token_color)));
  }

  ListItem::new(Line::from(spans))
}

/// Determines the color for token count display based on a three-tier system.
/// Provides visual feedback about token density.
fn get_token_count_color(token_count: usize) -> Color {
  if token_count < 1_000 {
    Color::Green // low token count - green
  } else if token_count < 10_000 {
    Color::Yellow // medium token count - yellow
  } else {
    Color::Red // high token count - red
  }
}

/// Handles keyboard input for file tree.
/// Returns true if input was handled, false otherwise.
pub fn handle_file_tree_input(app_state: &mut AppState, key: crossterm::event::KeyEvent) -> bool {
  use crossterm::event::KeyCode;

  match key.code {
    // nav keys with wrapping (up/down or k/j)
    KeyCode::Up | KeyCode::Char('k') => {
      if app_state.visible_paths.is_empty() {
        // no files to navigate
      } else if app_state.selected_index == 0 {
        // wrap to bottom
        app_state.selected_index = app_state.visible_paths.len() - 1;
      } else {
        app_state.selected_index -= 1;
      }
      true
    }
    KeyCode::Down | KeyCode::Char('j') => {
      if app_state.visible_paths.is_empty() {
        // no files to navigate
      } else if app_state.selected_index >= app_state.visible_paths.len() - 1 {
        // wrap to top
        app_state.selected_index = 0;
      } else {
        app_state.selected_index += 1;
      }
      true
    }

    // selection (space only, removed enter)
    KeyCode::Char(' ') => {
      if let Some(selected_path) = app_state.visible_paths.get(app_state.selected_index) {
        handle_selection_key(app_state, selected_path.clone());
      }
      true
    }

    // expansion/collapse (h/l and left/right arrows)
    KeyCode::Char('h') | KeyCode::Left => {
      // collapse directory
      if let Some(selected_path) = app_state.visible_paths.get(app_state.selected_index)
        && let Some(node) = app_state.file_tree.get_mut(selected_path)
        && node.is_directory
        && node.is_expanded
      {
        node.toggle_expansion();
        update_visible_files(app_state);
      }
      true
    }

    KeyCode::Char('l') | KeyCode::Right => {
      // expand directory
      if let Some(selected_path) = app_state.visible_paths.get(app_state.selected_index)
        && let Some(node) = app_state.file_tree.get_mut(selected_path)
        && node.is_directory
        && !node.is_expanded
      {
        node.toggle_expansion();
        update_visible_files(app_state);
      }
      true
    }

    // quit (q)
    // !!!
    KeyCode::Char('q') | KeyCode::Esc => {
      // Note: quit handling is now in App struct
      false // let app handle quit (q or esc)
    }

    _ => false,
  }
}

/// Handles selection key press for file and directory selection.
fn handle_selection_key(app_state: &mut AppState, selected_path: PathBuf) {
  if let Some(_node) = app_state.file_tree.get(&selected_path) {
    // toggle selection for both files and directories
    if crate::file_utils::toggle_selection_recursive(&mut app_state.file_tree, &selected_path).is_err() {
      // silently handle errors - don't block UI ops
    }
  }
}

/// Updates the visible files list based on current expansion states.
/// rebuilds the flattened tree view that gets displayed.
fn update_visible_files(app_state: &mut AppState) {
  app_state.visible_paths = crate::file_utils::flatten_visible_tree(&app_state.file_tree, &app_state.root_path);

  // make sure selected index is still valid (if not, set to last index)
  if app_state.selected_index >= app_state.visible_paths.len() {
    app_state.selected_index = app_state.visible_paths.len().saturating_sub(1);
  }
}

/// Renders the file list without borders (for use inside other blocks).
fn render_file_list_inner(frame: &mut Frame, area: Rect, app_state: &AppState, list_state: &mut ListState) {
  let dir_descendants_map = &app_state.dir_descendants_map;

  // get the currently highlighted index
  let highlighted_index = if !app_state.visible_paths.is_empty() {
    Some(app_state.selected_index.min(app_state.visible_paths.len() - 1))
  } else {
    None
  };

  // convert visible files to list items with proper formatting
  let items: Vec<ListItem> = app_state
    .visible_paths
    .iter()
    .enumerate()
    .map(|(index, path)| {
      let is_highlighted = highlighted_index == Some(index);
      create_list_item(
        path,
        &app_state.file_tree,
        &app_state.individual_token_counts,
        dir_descendants_map,
        is_highlighted,
      )
    })
    .collect();

  let files_list = List::new(items).highlight_style(Style::default().bg(Color::Blue)).highlight_symbol("► ");

  // make sure selected index is within bounds
  if !app_state.visible_paths.is_empty() {
    let selected = app_state.selected_index.min(app_state.visible_paths.len() - 1);
    list_state.select(Some(selected));
  } else {
    list_state.select(None);
  }

  frame.render_stateful_widget(files_list, area, list_state);
}
