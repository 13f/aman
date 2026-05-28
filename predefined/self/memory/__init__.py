"""
Memory package — extraction strategies and memory organization.
"""

from .extraction import (
    ExtractionStrategy,
    SESSION_EXTRACTION,
    USER_PREFERENCE_EXTRACTION,
    ERROR_PATTERN_EXTRACTION,
    BUILTIN_STRATEGIES,
    get_strategy,
    register_strategy,
    should_extract,
)

__all__ = [
    "ExtractionStrategy",
    "SESSION_EXTRACTION",
    "USER_PREFERENCE_EXTRACTION",
    "ERROR_PATTERN_EXTRACTION",
    "BUILTIN_STRATEGIES",
    "get_strategy",
    "register_strategy",
    "should_extract",
]
