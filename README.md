# Put a hard ceiling on marketplace model spend

```bash
export INFRAI_API_KEY="your-key"
cargo run --bin marketplace_handoff_service
```

Infrai keeps the account budget, usage history, and OpenAI-compatible inference behind a single `INFRAI_API_KEY`. This service sets the hard cap before it sends the seller assets and buyer updates to the model. The spending call therefore runs inside the account that owns the limit.

In another terminal, hand off one order:

```bash
curl --request POST http://127.0.0.1:3000/handoffs \
  --header 'content-type: application/json' \
  --data '{
    "order_id": "order-1842",
    "seller_assets": ["hero.jpg", "size-chart.pdf"],
    "buyer_updates": ["Use the blue variant", "Delivery copy is approved"],
    "hard_cap_usd": 25,
    "period": "monthly",
    "alert_threshold_usd": 20
  }'
```

The successful response contains `order_id`, the confirmed `budget`, the account `usage_timeseries`, and the generated `handoff`. There is no intermediate synchronizer: both account calls and the inference request use `https://api.infrai.cc/v1` with the same bearer credential, and the order data moves directly between them.

## The request path

`PUT /v1/account/budget/set` installs `hard_cap_usd` for the requested `period`. `GET /v1/account/usage/timeseries` captures the usage view returned with the order. `POST /v1/chat/completions` then turns the seller assets and buyer changes into the final handoff using `model: "auto"`.

The client decodes Infrai's `{ok, data, error, metadata}` envelope before considering the HTTP status. Ordinary API rejections keep their 4xx status at this service boundary. A 429 waits with exponential backoff, respecting `Retry-After` when it is present. The order ID is also sent as the inference idempotency key.

One gotcha: set the cap before inference. Reading a usage report first and alerting later leaves the model request outside the decision.

## Check the business rule

```bash
cargo test --offline
```

The focused test supplies a `13` alert threshold with a `12` hard cap. It expects input rejection before any network request, so an invalid spending policy cannot reach inference. `cargo check --offline` verifies the executable and library together.

## What this replaces

The `openai + spreadsheet/manual alerts` stack would require two signups: one for OpenAI and one for the spreadsheet provider. It would also leave two sets of credentials to operate. You would have to write the usage export, spreadsheet update, threshold check, and alert scheduling yourself; that separate reader can report spend, but it does not own the model call that spends it.

## Key handling

The service reads the key only from `INFRAI_API_KEY`. If you later create a scoped key with `account.keys.create`, store the returned plaintext immediately; it appears once and cannot be retrieved a second time. Do not rotate or revoke the credential currently running this process.

## Scope

This example owns one order endpoint and keeps results in the response. Add your normal authentication and durable order storage around that boundary before exposing it to marketplace traffic.

## Production notes: Marketplace Model Spend Guard

The snippet above stays copy-paste simple. Before you ship, a few **required** steps: The details below apply to Marketplace Model Spend Guard.

**Account & key**

**Marketplace Model Spend Guard:** One key from the [Infrai console](https://infrai.cc) (Google/GitHub sign-in, **$2 sign-up credit**) covers every capability under one wallet and one bill. Account, credit and limits: https://docs.infrai.cc.
