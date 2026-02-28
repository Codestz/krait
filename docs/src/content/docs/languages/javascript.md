---
title: JavaScript
description: Using krait with JavaScript projects.
---

**Language server:** [vtsls](https://github.com/yioneko/vtsls)

JavaScript shares the same language server as TypeScript (vtsls). It provides type inference even for plain `.js` files.

## Setup

```bash
npm install -g @vtsls/language-server
```

## Requirements

- A `package.json` at project root
- Optional: `jsconfig.json` for better type inference

## With JSDoc Types

vtsls uses JSDoc annotations for type inference in JavaScript files:

```js
/**
 * @param {string} name
 * @returns {Promise<User>}
 */
async function getUser(name) { ... }
```

`krait hover getUser` will show the inferred types.

## Supported Operations

- `find symbol`, `list symbols`, `read symbol`
- `hover` — inferred types from JSDoc
- `check` — JS semantic errors
- `edit replace`, `insert-after`, `insert-before`
- `rename`
