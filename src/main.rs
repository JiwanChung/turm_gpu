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
    partitions: Vec<String>,
    cpus: u32,           
    alloc_cpus: u32,     
}

fn extract_gpu_info(node: &Node) -> (u32, u32) {
    let total_gpus = node
        .gres
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|gres_entry| {
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

    let allocated_gpus = node
        .gres_used
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|gres_entry| {
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

fn is_node_fully_allocated(node: &Node, gpu_only_mode: bool) -> bool {
    let (allocated_gpus, total_gpus) = extract_gpu_info(node);
    let is_gpus_fully_allocated = total_gpus == 0 || allocated_gpus == total_gpus;
    
    if gpu_only_mode && total_gpus > 0 {
        is_gpus_fully_allocated
    } else {
        let is_cpus_fully_allocated = node.alloc_cpus == node.cpus;
        is_gpus_fully_allocated && is_cpus_fully_allocated
    }
}

fn load_nodes_from_command() -> Result<Vec<Node>, Box<dyn std::error::Error>> {
    let output = Command::new("scontrol")
        .arg("show")
        .arg("nodes")
        .arg("--json")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Command failed with error: {}", stderr);
        return Err("Failed to execute scontrol command".into());
    }

    let data = String::from_utf8_lossy(&output.stdout);
    let scontrol_output: ScontrolOutput = serde_json::from_str(&data)?;
    Ok(scontrol_output.nodes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut nodes = load_nodes_from_command()?;
    let mut scroll = 0;
    let refresh_interval = Duration::from_secs(5);
    let mut last_refresh = Instant::now();
    let mut gpu_only_mode = true;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut hide_no_free_gpus = false;
    let mut group_by_partitions = false;

    loop {
        let size = terminal.size()?;
        let rows_per_page = (size.height as usize).saturating_sub(5);

        let filtered_nodes: Vec<&Node> = if hide_no_free_gpus {
            nodes
                .iter()
                .filter(|node| {
                    let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                    if gpu_only_mode {
                        // GPU-only 모드일 때는 GPU 상태만 체크
                        if total_gpus > 0 {
                            (total_gpus - allocated_gpus) > 0
                        } else {
                            node.alloc_cpus < node.cpus
                        }
                    } else {
                        // 기존 모드에서는 GPU와 CPU 모두 체크
                        (total_gpus - allocated_gpus) > 0 || node.alloc_cpus < node.cpus
                    }
                })
                .collect()
        } else {
            nodes.iter().collect()
        };

        let grouped_nodes = if group_by_partitions {
            let mut partition_map: HashMap<String, Vec<&Node>> = HashMap::new();
            for node in &filtered_nodes {
                for partition in &node.partitions {
                    partition_map
                        .entry(partition.clone())
                        .or_insert_with(Vec::new)
                        .push(*node);
                }
            }
            let mut partition_list: Vec<(String, Vec<&Node>)> = partition_map.into_iter().collect();
            partition_list.sort_by(|a, b| a.0.cmp(&b.0));
            Some(partition_list)
        } else {
            None
        };

        let total_rows = if let Some(grouped_nodes) = &grouped_nodes {
            grouped_nodes.iter().map(|(_, nodes)| nodes.len() + 1).sum()
        } else {
            filtered_nodes.len()
        };

        let max_scroll = total_rows.saturating_sub(rows_per_page);
        scroll = scroll.min(max_scroll);

        terminal.draw(|f| {
            let title = format!(
                "Resource Allocation (Up/Down or k/j to scroll, 'f' to toggle free node filtering, 's' to toggle grouping by partitions, 'c' to toggle GPU-only mode [{}], 'q' to quit)",
                if gpu_only_mode { "ON" } else { "OFF" }
            );
            
            let block = Block::default()
                .title(title)
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
                    let header_cells = vec![
                        Cell::from(partition_name.clone())
                            .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                        Cell::from(""), Cell::from(""), Cell::from(""),
                        Cell::from(""), Cell::from(""), Cell::from(""),
                    ];
                    table_rows.push(Row::new(header_cells));

                    for node in nodes_in_partition {
                        let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                        let free_gpus = total_gpus - allocated_gpus;
                        let free_cpus = node.cpus - node.alloc_cpus;
                        let is_fully_allocated = is_node_fully_allocated(node, gpu_only_mode);

                        let partition_cell = Cell::from("");
                        let mut name_cell = Cell::from(node.name.clone()).style(Style::default().fg(Color::Green));
                        if is_fully_allocated {
                            name_cell = name_cell.style(Style::default().fg(Color::Red));
                        }

                        let free_gpu_cell = Cell::from(free_gpus.to_string());
                        let alloc_gpu_cell = Cell::from(allocated_gpus.to_string());
                        let total_gpu_cell = Cell::from(total_gpus.to_string());
                        let cpu_usage_cell = Cell::from(format!("{}/{}", node.alloc_cpus, node.cpus));
                        let free_cpu_cell = Cell::from(free_cpus.to_string());

                        let styled_free_gpu_cell = if free_gpus > 0 {
                            free_gpu_cell.style(Style::default().fg(Color::Green))
                        } else {
                            free_gpu_cell
                        };

                        let styled_free_cpu_cell = if free_cpus > 0 {
                            free_cpu_cell.style(Style::default().fg(Color::Green))
                        } else {
                            free_cpu_cell
                        };

                        table_rows.push(Row::new(vec![
                            partition_cell,
                            name_cell,
                            styled_free_gpu_cell,
                            alloc_gpu_cell,
                            total_gpu_cell,
                            cpu_usage_cell,
                            styled_free_cpu_cell,
                        ]));
                    }
                }
            } else {
                for node in &filtered_nodes {
                    let (allocated_gpus, total_gpus) = extract_gpu_info(node);
                    let free_gpus = total_gpus - allocated_gpus;
                    let free_cpus = node.cpus - node.alloc_cpus;
                    let is_fully_allocated = is_node_fully_allocated(node, gpu_only_mode);

                    let partition_cell = Cell::from(node.partitions.join(", ")).style(Style::default().fg(Color::Blue));
                    let mut name_cell = Cell::from(node.name.clone()).style(Style::default().fg(Color::Green));
                    if is_fully_allocated {
                        name_cell = name_cell.style(Style::default().fg(Color::Red));
                    }

                    let free_gpu_cell = Cell::from(free_gpus.to_string());
                    let alloc_gpu_cell = Cell::from(allocated_gpus.to_string());
                    let total_gpu_cell = Cell::from(total_gpus.to_string());
                    let cpu_usage_cell = Cell::from(format!("{}/{}", node.alloc_cpus, node.cpus));
                    let free_cpu_cell = Cell::from(free_cpus.to_string());

                    let styled_free_gpu_cell = if free_gpus > 0 {
                        free_gpu_cell.style(Style::default().fg(Color::Green))
                    } else {
                        free_gpu_cell
                    };

                    let styled_free_cpu_cell = if free_cpus > 0 {
                        free_cpu_cell.style(Style::default().fg(Color::Green))
                    } else {
                        free_cpu_cell
                    };

                    table_rows.push(Row::new(vec![
                        partition_cell,
                        name_cell,
                        styled_free_gpu_cell,
                        alloc_gpu_cell,
                        total_gpu_cell,
                        cpu_usage_cell,
                        styled_free_cpu_cell,
                    ]));
                }
            }

            let displayed_rows: Vec<(usize, Row)> = table_rows
                .into_iter()
                .enumerate()
                .skip(scroll)
                .take(rows_per_page)
                .collect();

            let rows = displayed_rows.into_iter().map(|(index, mut row)| {
                let bg_color = if (scroll + index) % 2 == 0 {
                    Color::Reset
                } else {
                    Color::Rgb(40, 40, 40)
                };
                row = row.style(Style::default().bg(bg_color));
                row
            });

            let header_cells = ["Partitions", "Node", "Free GPUs", "Alloc GPUs", "Total GPUs", "CPU Usage", "Free CPUs"]
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
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(10),
                ])
                .column_spacing(1);

            f.render_widget(table, layout[0]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key_event) => match key_event.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('f') => {
                        hide_no_free_gpus = !hide_no_free_gpus;
                        scroll = 0;
                    }
                    KeyCode::Char('s') => {
                        group_by_partitions = !group_by_partitions;
                        scroll = 0;
                    }
                    KeyCode::Char('c') => {
                        gpu_only_mode = !gpu_only_mode;
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
                    _ => {}
                },
                _ => {}
            }
        }

        if last_refresh.elapsed() >= refresh_interval {
            nodes = load_nodes_from_command()?;
            last_refresh = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}