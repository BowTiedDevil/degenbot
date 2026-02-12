# Lessons Learned

When you learn critical information, such as how to call a tool, add it to this file with a note about how it was discovered

## Database Operations

**NEVER reset the database** unless explicitly requested by the user. Database resets destroy all accumulated data including:
- Token and pool registries
- User positions and balances
- Historical event processing state

Operations that interact with the database will perform validation prior to committing, so the data is durable and accurate. If a validation fails, assume that the dirty data is bad, not the database.

When investigating Aave processing errors, use verbose logging flags to trace the issue:
- `DEGENBOT_VERBOSE_USERS=0x123...` - Trace specific user addresses
- `DEGENBOT_VERBOSE_TX=0xabc...` - Trace specific transactions
- `DEGENBOT_VERBOSE_ALL=1` - Enable all verbose logging