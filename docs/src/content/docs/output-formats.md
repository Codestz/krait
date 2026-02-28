---
title: Output Formats
description: compact, json, and human output formats.
---

Krait supports three output formats via `--format`:

```bash
krait find symbol Foo              # compact (default)
krait find symbol Foo --format json
krait find symbol Foo --format human
```

## compact (default)

Optimized for LLM context windows. Minimal tokens, maximum information density.

```
# find symbol
fn createOrder  src/orders/service.ts:42

# list symbols
fn createOrder [42]
fn cancelOrder [67]
class OrderService [12]
  fn constructor [13]
  fn validateItems [28]

# check
error src/orders/service.ts:45:12 TS2339 Property 'id' does not exist on type 'never'
warning src/orders/service.ts:67:5 TS6133 'result' is declared but never read
2 errors, 1 warning

# hover
class OrderService extends BaseService<Order>
Manages the full order lifecycle.
src/orders/service.ts:12
```

## json

Structured JSON output for programmatic consumption.

```json
{
  "kind": "find_symbol",
  "results": [
    {
      "name": "createOrder",
      "kind": "function",
      "path": "src/orders/service.ts",
      "line": 42,
      "column": 0
    }
  ]
}
```

## human

Verbose, human-readable format for terminal use.

```
Symbol: createOrder
  Kind:    function
  File:    src/orders/service.ts
  Line:    42
  Column:  0
```

## Choosing a Format

| Use case | Format |
|----------|--------|
| AI agent context | `compact` (default) |
| Programmatic parsing | `json` |
| Manual browsing | `human` |
| CI scripts | `compact` or `json` |
