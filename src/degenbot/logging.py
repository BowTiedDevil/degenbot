# ruff: noqa: A005

import logging

"""
Create a global logger instance for this module.
"""

logger = logging.getLogger("degenbot")
logger.propagate = False
logger.setLevel(logging.INFO)
logger.addHandler(logging.StreamHandler())
