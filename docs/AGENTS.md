---
title: Documentation Guidelines
category: meta
tags:
  - documentation
  - agents
complexity: standard
---

# Documentation Guidelines

## Overview

Documentation in `docs/` serves both human readers and AI agents. Follow these conventions to ensure consistency and usefulness for both audiences.

## File Structure

All documentation files must have YAML frontmatter:

```yaml
---
title: Document Title
category: cli | uniswap | curve | aave | arbitrage | erc20 | database | registry | types | meta
tags:
  - tag1
  - tag2
related_files:
  - ../../src/degenbot/path/to/file.py
complexity: simple | standard | complex | architectural
---
```

## Content Organization

Structure documents in this order:

1. **Overview** - What this module does (2-3 sentences)
2. **Background** - Context needed before details (optional for simple docs)
3. **Main Sections** - Detailed content organized by topic
4. **Key Concepts** - Important patterns, invariants, gotchas
5. **See Also** - Related documentation and source files

## Formatting

**Links**
- Files: `[filename](../../src/degenbot/path/to/file.py)`
- Directories: `[dirname](../../src/degenbot/path/to/dir/)`
- Documents: `[title](./other-doc.md)`

**Code Blocks**
- Always specify language: ```python, ```bash, ```solidity
- No line numbers in code examples
- Include imports in Python examples if non-obvious

**Visual Elements**
- Use mermaid diagrams for data flow and architecture
- Use tables for parameter/option documentation
- Use inline code for file paths, function names, and variable references

## Maintenance

- Update docs when behavior changes in referenced files
- Keep `related_files` list current
- Complexity should reflect implementation complexity, not documentation length
- Re-generate mermaid diagrams if the represented logic changes

## Styling for Human Readers

When writing documentation:

- **Use complete sentences** - Not telegraphic style like AGENTS.md
- **Explain "why" not just "what"** - Context helps humans understand
- **Include examples** - Show usage, not just API reference
- **Progressive disclosure** - Overview first, details available
- **Cross-reference liberally** - Help readers navigate related concepts

## See Also

- [AGENTS.md](../AGENTS.md) - AI-only coding standards
- [database.md](./cli/database.md) - Example of standard complexity documentation
