# Lessons Learned

When you learn critical information, such as how to call a tool, add it to this file with a note about how it was discovered

## Database Operations

**NEVER reset the database** unless explicitly requested by the user. Database resets destroy all accumulated data including:
- Token and pool registries
- User positions and balances
- Historical event processing state

Operations that interact with the database will perform validation prior to committing, so the data is durable and accurate. If a validation fails, assume that the uncommitted data is bad, not the database.
