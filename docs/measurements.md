# Local measurements

Measured October 17, 2026 on this Mac: Apple M4 Pro, 24 GiB RAM.

## File awareness

`cargo run --release -p jarvis --example awareness_latency` creates 100 generated text files, starts the actual file observer, and performs 20 sequential edits. Each sample measures the write through notification/debounce to visibility through a separate SQLite connection, polled every 2 ms.

| Measure | Release run |
|---|---:|
| Startup through initial indexing and observer readiness | 32.75 ms |
| Write-to-index median | 168.25 ms |
| Write-to-index p95 | 171.08 ms |
| Write-to-index maximum | 172.53 ms |
| Peak resident set reported by `/usr/bin/time -l` | 10,567,680 bytes |
| Main database file before connection close | 393,216 bytes |

The observer uses a 150 ms debounce. This is a synthetic file-awareness measurement, not voice-to-answer or total assistant latency. RSS describes the benchmark process, not the panel, browser, or speech model. Database bytes exclude WAL/SHM files. An independent debug run observed a 168.23 ms median and 172.68 ms p95.

## Live provider verification

The configured Jev key successfully routed a benign request through the production API. Crusoe's authenticated model catalog returned `nvidia/Nemotron-3.5-Lightning-30B-A3B`; this case-sensitive ID differs from the lowercase label in its public documentation. A production chat request completed with the selected model after setting `chat_template_kwargs.enable_thinking=false` for direct conversational answers.

These checks establish live connectivity and response parsing. They are not a statistically meaningful inference benchmark or a comparison against another assistant.
