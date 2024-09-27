<img alt="turm demo" src="pic.png" width="100%" />

`turm_gpu` is a rust-based TUI for inspecting GPU resources in a slurm enviroment.
It internally uses `scontrol` for the data retrieval.

## Installation

If you know `pip`,

```bash
pip install turm-gpu
```

or use `pipx`.

Alternatively, if you know `cargo`,
```bash
git clone https://github.com/JiwanChung/turm_gpu
cd turm_gpu
cargo install --path . 
```

## Usage

```bash
turm_gpu
```

The rest should be straightforward.
