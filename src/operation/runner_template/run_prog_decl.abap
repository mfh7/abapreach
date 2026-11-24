
    TYPES:
      BEGIN OF ty_col,
        name    TYPE string,
        text    TYPE string,
        visible TYPE abap_bool,
        cfield  TYPE string,
        qfield  TYPE string,
        cfixed  TYPE string,
        qfixed  TYPE string,
      END OF ty_col,
      ty_cols TYPE STANDARD TABLE OF ty_col WITH DEFAULT KEY,
      BEGIN OF ty_known,
        name TYPE string,
        kind TYPE string,
      END OF ty_known,
      ty_knowns TYPE STANDARD TABLE OF ty_known WITH DEFAULT KEY.

    DATA:
      mt_sel  TYPE STANDARD TABLE OF rsparams WITH DEFAULT KEY,
      mt_cols TYPE ty_cols.

    METHODS run_prog.

    METHODS check_report
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS source_hash
      IMPORTING
        it_source      TYPE string_table
      RETURNING
        VALUE(rv_hash) TYPE string
      RAISING
        cx_abap_message_digest.

    METHODS selection_names
      EXPORTING
        et_known TYPE ty_knowns
        ev_ok    TYPE abap_bool.

    METHODS build_selection
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS capture_alv.

    METHODS find_columns
      IMPORTING
        is_meta TYPE any.

    METHODS columns_from_row
      IMPORTING
        is_row TYPE any.

    METHODS emit_columns.

    METHODS emit_row
      IMPORTING
        is_row       TYPE any
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS cell_text
      IMPORTING
        is_row         TYPE any
        is_col         TYPE ty_col
      RETURNING
        VALUE(rv_text) TYPE string.

    METHODS comp_text
      IMPORTING
        is_row         TYPE any
        iv_name        TYPE string
      RETURNING
        VALUE(rv_text) TYPE string.

    METHODS print_list.
