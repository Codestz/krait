---
title: Editing Commands
description: Semantic editing commands — edit by name, not line number.
---

All edit commands read the new code from **stdin**. This makes them composable with code generators, formatters, or other tools.

## edit replace

Replace a symbol's entire body.

```bash
cat new_impl.rs | krait edit replace <symbol>
```

**Example:**
```bash
cat updated_service.ts | krait edit replace OrderService
```

The LSP locates the symbol's exact start and end, including its closing brace. The new code from stdin replaces everything in between.

## edit insert-after

Insert code after a symbol's closing brace.

```bash
echo 'fn helper() {}' | krait edit insert-after <symbol>
```

## edit insert-before

Insert code before a symbol's declaration.

```bash
echo '#[derive(Debug)]' | krait edit insert-before MyStruct
```

## rename

Cross-file rename using LSP `workspace/rename`. Updates all references automatically.

```bash
krait rename <old-name> <new-name>
```

## fix

Apply LSP quick fixes to a file.

```bash
krait fix [path]
```

## format

Run the LSP formatter on a file.

```bash
krait format <path>
```

## Stdin Patterns

```bash
# From a file
cat new_body.rs | krait edit replace MyStruct

# From a heredoc
krait edit replace MyStruct << 'EOF'
pub struct MyStruct {
    name: String,
}
EOF

# From a command
generate-code --symbol MyStruct | krait edit replace MyStruct
```

> **Tip:** Avoid `echo` for multi-line content — use heredoc or pipe from a file.
