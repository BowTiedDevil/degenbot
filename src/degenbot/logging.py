import logging
import os
import sys

"""
Create a global logger instance.
"""

logger = logging.getLogger(__name__)
logger.propagate = False

# Check DEGENBOT_DEBUG environment variable for debug mode
if os.environ.get("DEGENBOT_DEBUG", "").lower() in {"1", "true", "yes"}:
    logger.setLevel(logging.DEBUG)
else:
    logger.setLevel(logging.INFO)

logger.addHandler(logging.StreamHandler(sys.stdout))
