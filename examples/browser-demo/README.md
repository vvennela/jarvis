# Browser awareness demo

A synthetic project-priority page for demonstrating fresh browser context.

```sh
python3 examples/browser-demo/serve.py
```

1. In the Jarvis extension settings, approve `http://localhost`. Chrome grants the host across ports; enter the address without a port.
2. Open `http://localhost:8765` in Arc or Chrome.
3. Ask Jarvis: “What is the highest-priority project on my browser demo page?”
4. Edit the priority, click **Publish visible priority**, and ask again.

The visible priority starts with Atlas and can change to Beacon. The input field and marked private fixture stay outside captured page text. The server binds to loopback and serves only its three bundled assets.

Check bridge setup with `target/debug/jarvis browser-status`.
