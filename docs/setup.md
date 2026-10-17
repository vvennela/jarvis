# Setup contract

These are the requirements for planned live integrations. The foundation's offline tests need only Rust and Cargo; configuring credentials does not make unimplemented adapters available.

## Exactly which API keys?

1. **`TYPESAFE_API_KEY`** from the [TypeSafe console](https://console.typesafe.ai/). The Rust adapter will send bearer-authenticated requests to `https://api.typesafe.ai/v1/systemone` with model `jev-latest`. Jev evaluates structured choice/score questions; it is not a drop-in chat-completions model. See the [official quickstart](https://docs.typesafe.ai/introduction/quickstart).
2. **`CRUSOE_API_KEY`** with access to Crusoe serverless inference. Base URL: `https://api.inference.crusoecloud.com/v1`. Use the authenticated model catalog to select exact available IDs and set `JARVIS_CONVERSATION_MODEL` and `JARVIS_WORKER_MODEL`. Prefer an available Nemotron model for the hackathon integration, but do not claim access before the account is tested. Use [Crusoe's documentation](https://docs.crusoecloud.com/) to verify account setup and currently supported APIs.

One Crusoe key can serve both conversation and worker roles if its account permissions allow both selected models. Do not request an additional key just because there are two roles.

**Not required:** OpenAI, Anthropic, ElevenLabs, Wispr, Gmail, Spotify, GitHub API tokens, or a wake-word service key. Optional future provider substitutions may introduce their own credentials; they are not part of today's required setup.

Store secrets locally in an ignored `.env` or macOS Keychain once that integration exists. Never send actual values in chat, paste them into source, include them in fixtures, or print authorization headers. `.env.example` contains only placeholders. The foundation does not yet promise automatic dotenv loading.

## Local prerequisites, not keys

- Rust stable/Cargo and macOS developer command-line tools for native builds.
- A compatible local Whisper model file, referenced by `JARVIS_WHISPER_MODEL_PATH`; keep weights outside the repository. Confirm its license and resource requirements before distributing it.
- Microphone permission for capture. Push-to-talk is the first supported interaction target.
- Accessibility permission for native focused-window/selection observation; denied permissions produce a partial-awareness state rather than fake context.
- Chrome extension installation and Native Messaging host registration. Only approved origins and the exact paired extension can use the bridge.
- User-selected Documents/Downloads access. The initial demo can target dedicated subdirectories for a bounded, reproducible session.
- Existing signed-in browser sessions for any later supported web adapter. Do not export cookies to the model or ask users for account passwords.

Git authentication is separate and already belongs to the user's configured git transport. Use the supplied repository URL with git; do not invoke GitHub CLI or provision API credentials.

## First live verification, once adapters exist

Validate keys with one minimal non-private request to each provider. Discover models, select IDs, and verify conversation output before wiring a worker. Check permission-denied behavior before granting permissions. Pair the extension, observe a local fixture page, and test stale/disconnected status. Then add voice. No purchasing, sending or deleting is part of setup verification.
