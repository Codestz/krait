---
title: Quick Start
description: Get up and running with krait in minutes.
---

## Navigate Your Code

```bash
cd your-project

krait find symbol MyStruct         # locate definition
krait list symbols src/lib.rs      # semantic outline
krait read symbol MyStruct         # extract full body
```

## Understand APIs

```bash
krait hover MyStruct               # type info + docs
krait check                        # LSP diagnostics
```

## Edit Semantically

```bash
# Replace a symbol body via stdin
cat new_impl.rs | krait edit replace MyStruct

# Insert code around a symbol
echo 'fn helper() {}' | krait edit insert-after MyStruct

# Cross-file rename
krait rename OldName NewName
```

## Fix and Format

```bash
krait fix                          # apply LSP quick fixes
krait format src/lib.rs            # run LSP formatter
```

## Agent Workflow Example

Here's a typical agent workflow for fixing a bug:

```bash
krait find symbol PaymentProcessor    # 1. Locate logic
krait read symbol PaymentProcessor    # 2. Read the body
krait find refs PaymentProcessor      # 3. Check impact
krait check src/payments.ts           # 4. Identify errors
cat fix.ts | krait edit replace PaymentProcessor  # 5. Apply fix
krait check src/payments.ts           # 6. Verify
```
