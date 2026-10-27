"""Error type shared by the backend, the server protocol and the CLI."""


class PreviewError(Exception):
    """An error that is reported to the user with a stable machine-readable code.

    Codes: not_found, not_a_file, invalid_image, invalid_nifti, unsupported_format,
    unsupported_datatype, corrupt_data, too_large, bad_request, internal.
    """

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message

    def to_dict(self) -> dict:
        return {"code": self.code, "message": self.message}
