use serde::Deserialize;
use std::time::Instant;
use tui::{
    backend::CrosstermBackend,
    widgets::{Block, Borders, Row, Table, Cell},
    layout::{Constraint, Layout, Direction},
    style::{Style, Color, Modifier},
    Terminal,
};
use crossterm::{
    execute,
    event,
    terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    event::{Event, KeyCode}
};
use std::collections::HashMap;
use std::process::Command;
use core::time::Duration;
use std::cmp::min;


#[derive(Deserialize, Debug)]
struct ScontrolOutput {
    nodes: Vec<Node>,
}

#[derive(Deserialize, Debug)]
struct Node {
    name: String,
    gres: Option<String>,
    gres_used: Option<String>,
    partitions: Vec<String>
    // Add other fields if necessary
}

fn extract_gpu_info(node: &Node) -> (u32, u32) {
    let total_gpus = node
        .gres
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|gres_entry| {
            // Split the gres entry by colons
            let parts: Vec<&str> = gres_entry.split(':').collect();
            // We expect at least 3 parts: ["gres", "gpu_model", "gpu_info"]
            if parts.len() >= 3 {
                // The third part contains the number of GPUs and possibly IDs, e.g., "8(1-8)"
                let gpu_info = parts[2];
                // Extract the number before any parentheses
                if let Some((number_str, _)) = gpu_info.split_once('(') {
                    number_str.parse::<u32>().ok()
                } else {
                    // If there are no parentheses, attempt to parse the whole string
                    gpu_info.parse::<u32>().ok()
                }
            } else {
                None
            }
        })
        .sum();

    let allocated_gpus = node
        .gres_used
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|gres_entry| {
            // Similar parsing logic as above
            let parts: Vec<&str> = gres_entry.split(':').collect();
            if parts.len() >= 3 {
                let gpu_info = parts[2];
                if let Some((number_str, _)) = gpu_info.split_once('(') {
                    number_str.parse::<u32>().ok()
                } else {
                    gpu_info.parse::<u32>().ok()
                }
            } else {
                None
            }
        })
        .sum();

    (allocated_gpus, total_gpus)
}

fn load_nodes_from_command() -> Result<Vec<Node>, Box<dyn std::error::Error>> {
    let output = Command::new("scontrol")
        .arg("show")
        .arg("nodes")
        .arg("--json")
        .output()?;

    // Check if command was successful
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Command failed with error: {}", stderr);
        return Err("Failed to execute scontrol command".into());
    }

    // Convert output to string
    let data = String::from_utf8_lossy(&output.stdout);

    // Debug: Print the raw JSON data
    // println!("Raw JSON Output:\n{}", data);

    // Parse the JSON data
    let scontrol_output: ScontrolOutput = serde_json::from_str(&data)?;

    Ok(scontrol_output.nodes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load nodes
    let mut nodes = load_nodes_from_command()?;

    // Scroll offset
    let mut scroll = 0;

    let refresh_interval = Duration::from_secs(5);
    let mut last_refresh = Instant::now();

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut hide_no_free_gpus = false;
    // Variable to track grouping by partitions
    let mut group_by_partitions = false;

    // Draw the TUI
    loop {
        let size = terminal.size()?;

        let rows_per_page = (size.height as usize).saturating_sub(5); // Adjust for header, borders, and margins

        let filtered_nodes: Vec<&Node> = if hide_no_free_gpus {
            nodes
                .iter()
                .filter(|node| {
                    let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                    (total_gpus - allocated_gpus) > 0
                })
                .collect()
        } else {
            nodes.iter().collect()
        };

        // Group nodes by partitions if grouping is enabled
        let grouped_nodes = if group_by_partitions {
            // Create a HashMap to group nodes by partitions
            let mut partition_map: HashMap<String, Vec<&Node>> = HashMap::new();

            for node in &filtered_nodes {
                for partition in &node.partitions {
                    partition_map
                        .entry(partition.clone())
                        .or_insert_with(Vec::new)
                        .push(*node);
                }
            }

            // Convert the HashMap into a Vec and sort it by partition name
            let mut partition_list: Vec<(String, Vec<&Node>)> = partition_map.into_iter().collect();
            partition_list.sort_by(|a, b| a.0.cmp(&b.0));
            Some(partition_list)
        } else {
            None
        };

        let total_rows = if let Some(grouped_nodes) = &grouped_nodes {
            // Sum of partition headers and node counts
            grouped_nodes.iter().map(|(_, nodes)| nodes.len() + 1).sum()
        } else {
            filtered_nodes.len()
        };

        // Ensure the scroll offset is within valid bounds
        let max_scroll = total_rows.saturating_sub(rows_per_page);
        scroll = scroll.min(max_scroll);

        terminal.draw(|f| {

            let block = Block::default()
                .title("GPU Allocation (Use Up/Down or k/j to scroll, 'f' to toggle free node filtering, 's' to toggle grouping by partitions, 'q' to quit)")
                .borders(Borders::ALL);
            f.render_widget(block, size);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Percentage(100)].as_ref())
                .split(size);

            let mut table_rows: Vec<Row> = Vec::new();

            if let Some(grouped_nodes) = &grouped_nodes {
                for (partition_name, nodes_in_partition) in grouped_nodes {
                    // Create a partition header row
                    let header_cells = vec![
                        Cell::from(partition_name.clone())
                            .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                        Cell::from(""), Cell::from(""), Cell::from(""), Cell::from(""),
                    ];
                    table_rows.push(Row::new(header_cells));

                    // Add node rows
                    for node in nodes_in_partition {
                        let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                        let free_gpus = total_gpus - allocated_gpus;

                        // Create cells for each column
                        let partition_cell = Cell::from(""); // Empty since partition is shown in header
                        let name_cell = Cell::from(node.name.clone()).style(Style::default().fg(Color::Green));
                        let free_cell = Cell::from(free_gpus.to_string());
                        let alloc_cell = Cell::from(allocated_gpus.to_string());
                        let total_cell = Cell::from(total_gpus.to_string());

                        // Apply style to the free_cell if free_gpus > 0
                        let styled_free_cell = if free_gpus > 0 {
                            free_cell.style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                        } else {
                            free_cell
                        };

                        // Create a row with the cells
                        let row = Row::new(vec![
                            partition_cell,
                            name_cell,
                            styled_free_cell,
                            alloc_cell,
                            total_cell,
                        ]);

                        table_rows.push(row);
                    }
                }
            } else {
                // Not grouped, display nodes normally
                for node in &filtered_nodes {
                    let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                    let free_gpus = total_gpus - allocated_gpus;

                    // Create cells for each column
                    let partition_cell = Cell::from(node.partitions.join(", ")).style(Style::default().fg(Color::Blue));
                    let name_cell = Cell::from(node.name.clone()).style(Style::default().fg(Color::Green));
                    let free_cell = Cell::from(free_gpus.to_string());
                    let alloc_cell = Cell::from(allocated_gpus.to_string());
                    let total_cell = Cell::from(total_gpus.to_string());

                    // Apply style to the free_cell if free_gpus > 0
                    let styled_free_cell = if free_gpus > 0 {
                        free_cell.style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                    } else {
                        free_cell
                    };

                    // Create a row with the cells
                    let row = Row::new(vec![
                        partition_cell,
                        name_cell,
                        styled_free_cell,
                        alloc_cell,
                        total_cell,
                    ]);

                    table_rows.push(row);
                }
            }
            // Apply scrolling
            let displayed_rows: Vec<(usize, Row)> = table_rows
                .into_iter()
                .enumerate()
                .skip(scroll)
                .take(rows_per_page)
                .collect();

            // Generate rows with zebra striping
            let rows = displayed_rows.into_iter().map(|(index, mut row)| {
                // Determine background color for zebra striping
                let bg_color = if (scroll + index) % 2 == 0 {
                    Color::Reset
                } else {
                    Color::Rgb(40, 40, 40) // Adjust RGB values as needed
                };

                // Apply background color to the row
                row = row.style(Style::default().bg(bg_color));

                row
            });

            let header_cells = ["Partitions", "Node", "Free", "Alloc", "Total"]
                .iter()
                .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
            let header = Row::new(header_cells)
                .style(Style::default().fg(Color::Yellow));

            let table = Table::new(rows)
                .header(header)
                .block(Block::default().borders(Borders::ALL))
                .widths(&[
                    Constraint::Length(20),
                    Constraint::Length(15),
                    Constraint::Length(5),
                    Constraint::Length(5),
                    Constraint::Length(5),
                ])
                .column_spacing(1);

            f.render_widget(table, layout[0]);
        })?;

        // Handle input events
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key_event) => match key_event.code {
                    KeyCode::Char('q') => {
                        break; // Exit the loop to quit the application
                    }
                    KeyCode::Char('f') => {
                        hide_no_free_gpus = !hide_no_free_gpus;
                        // Reset scroll to 0 to prevent issues when the list changes
                        scroll = 0;
                    }
                    KeyCode::Char('s') => {
                        group_by_partitions = !group_by_partitions;
                        scroll = 0; // Reset scroll to prevent issues when the list changes
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        scroll = scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let max_scroll = nodes.len().saturating_sub(rows_per_page);
                        scroll = min(scroll + 1, max_scroll);
                    }
                    KeyCode::PageUp => {
                        scroll = scroll.saturating_sub(rows_per_page);
                    }
                    KeyCode::PageDown => {
                        let max_scroll = nodes.len().saturating_sub(rows_per_page);
                        scroll = min(scroll + rows_per_page, max_scroll);
                    }
                    KeyCode::Home => {
                        scroll = 0;
                    }
                    KeyCode::End => {
                        scroll = nodes.len().saturating_sub(rows_per_page);
                    }
                    _ => {} // Handle other keys if needed
                },
                _ => {}
            }
        }

        if last_refresh.elapsed() >= refresh_interval {
            nodes = load_nodes_from_command()?;
            last_refresh = Instant::now();
        }
    }

    // Clean up terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
        
