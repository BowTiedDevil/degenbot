class DegenbotError(Exception):
    """
    Base exception used as the parent class for all exceptions raised by this package.

    Calling code should catch `DegenbotError` and derived classes separately before general
    exceptions, e.g.:

    ```
    try:
        degenbot.some_function()
    except SpecificDegenbotError:
        ... # handle a specific exception
    except DegenbotError:
        ... # handle non-specific degenbot exception
    except Exception:
        ... # handle exceptions raised by 3rd party dependencies or Python built-ins
    ```

    An optional string-formatted message may be attached to the exception and retrieved by accessing
    the `.message` attribute.
    """

    message: str | None = None

    def __init__(self, message: str | None = None) -> None:
        if message:
            self.message = message
            super().__init__(message)


class DegenbotValueError(DegenbotError): ...


class DegenbotTypeError(DegenbotError): ...


class ExternalServiceError(DegenbotError):
    """
    Raised on errors resulting to some call to an external service.
    """

    def __init__(self, error: str) -> None:
        self.error = error
        super().__init__(message=f"External service error: {error}")
