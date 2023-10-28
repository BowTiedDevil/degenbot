import logging

"""
Create a shared logger instance for this module. Set default level to INFO and send to stdout with StreamHandler
"""

logger = logging.getLogger("degenbot")
logger.propagate = False
logger.setLevel(logging.INFO)
logger.addHandler(logging.StreamHandler())
