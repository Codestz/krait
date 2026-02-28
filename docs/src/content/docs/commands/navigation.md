---
title: Navigation Commands
description: Commands for finding and reading code.
---

## find symbol

Locate the definition of a symbol anywhere in your project.

```bash
krait find symbol <name>
```

**Example:**
```
$ krait find symbol createOrder
fn createOrder  src/orders/service.ts:42
```

## find refs

Find all usages of a symbol across the codebase.

```bash
krait find refs <name>
```

**Example:**
```
$ krait find refs createOrder
src/orders/handler.ts:18
src/orders/service.ts:42
src/tests/orders.test.ts:15
```

> **Note:** `find refs` scans all files in your project. On large codebases it may take 1-2s. Subsequent calls are faster.

## list symbols

Get a semantic outline of a file — functions, classes, types.

```bash
krait list symbols <path>
krait list symbols <path> --depth 2    # include methods/fields
```

**Example:**
```
$ krait list symbols src/orders/service.ts
class OrderService [12]
  fn constructor [13]
  fn validateItems [28]
  fn createOrder [42]
  fn cancelOrder [67]
```

## read file

Read a file with line numbers.

```bash
krait read file <path>
krait read file <path> --from 10 --to 50
```

## read symbol

Extract the body of a specific symbol.

```bash
krait read symbol <name>
krait read symbol <name> --signature-only    # declaration only
```

**Example:**
```
$ krait read symbol createOrder
async function createOrder(input: CreateOrderInput): Promise<Order> {
  const items = await this.validateItems(input.items);
  // ...
}
```

## hover

Get type information and documentation for a symbol.

```bash
krait hover <symbol>
```

**Example:**
```
$ krait hover OrderService
class OrderService extends BaseService<Order>
Manages the full order lifecycle including payment, fulfillment, and cancellation.
src/orders/service.ts:12
```
