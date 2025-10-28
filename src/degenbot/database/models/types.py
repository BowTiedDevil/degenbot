from typing import Annotated

from sqlalchemy import ForeignKey, String
from sqlalchemy.orm import mapped_column

PrimaryKeyInt = Annotated[
    int,
    mapped_column(primary_key=True, autoincrement=True),
]
PrimaryForeignKeyPoolId = Annotated[
    int,
    mapped_column(ForeignKey("pools.id"), primary_key=True),
]
PrimaryForeignKeyManagedPoolId = Annotated[
    int,
    mapped_column(ForeignKey("managed_pools.id"), primary_key=True),
]
ForeignKeyManagedPoolId = Annotated[
    int,
    mapped_column(ForeignKey("managed_pools.id"), index=True),
]
ForeignKeyPoolId = Annotated[
    int,
    mapped_column(ForeignKey("pools.id")),
]
ForeignKeyPoolManagerId = Annotated[
    int,
    mapped_column(ForeignKey("pool_managers.id"), index=True),
]
ForeignKeyTokenId = Annotated[
    int,
    mapped_column(ForeignKey("erc20_tokens.id"), index=True),
]
ManagedPoolHash = Annotated[
    str,
    mapped_column(String(66)),
]
