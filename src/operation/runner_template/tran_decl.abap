
    TYPES:
      BEGIN OF ty_tran,
        program  TYPE tstc-pgmna,
        dynpro   TYPE tstc-dypno,
        cinfo    TYPE tstc-cinfo,
        text     TYPE tstct-ttext,
        devclass TYPE tadir-devclass,
        author   TYPE tadir-author,
      END OF ty_tran.

    DATA:
      ms_tran    TYPE ty_tran,
      mt_live    TYPE string_table,
      mv_tcode   TYPE tstc-tcode,
      mv_objname TYPE tadir-obj_name.

    METHODS tran_live
      RETURNING
        VALUE(rv_found) TYPE abap_bool.

    METHODS tran_hash
      RETURNING
        VALUE(rv_hash) TYPE string.

    METHODS tran_read.

    METHODS tran_create.

    METHODS tran_insert.

    METHODS tran_undo
      IMPORTING
        iv_had_row TYPE abap_bool.

    METHODS tran_delete.

    METHODS tran_request
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS tran_remove.
