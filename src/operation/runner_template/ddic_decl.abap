
    TYPES:
      BEGIN OF ty_def,
        dd04v TYPE dd04v,
        dd01v TYPE dd01v,
        dd07v TYPE STANDARD TABLE OF dd07v WITH DEFAULT KEY,
        dd02v TYPE dd02v,
        dd09l TYPE dd09v,
        dd03p TYPE STANDARD TABLE OF dd03p WITH DEFAULT KEY,
        dd05m TYPE STANDARD TABLE OF dd05m WITH DEFAULT KEY,
        dd08v TYPE STANDARD TABLE OF dd08v WITH DEFAULT KEY,
        dd35v TYPE STANDARD TABLE OF dd35v WITH DEFAULT KEY,
        dd36m TYPE STANDARD TABLE OF dd36m WITH DEFAULT KEY,
        dd40v TYPE dd40v,
        dd42v TYPE STANDARD TABLE OF dd42v WITH DEFAULT KEY,
        dd43v TYPE STANDARD TABLE OF dd43v WITH DEFAULT KEY,
      END OF ty_def,
      BEGIN OF ty_user,
        object    TYPE trobjtype,
        name      TYPE ddobjname,
        is_transp TYPE abap_bool,
      END OF ty_user,
      ty_users TYPE SORTED TABLE OF ty_user WITH UNIQUE KEY object name,
      BEGIN OF ty_override,
        rollname TYPE rollname,
        domname  TYPE domname,
        datatype TYPE datatype_d,
        leng     TYPE ddleng,
        decimals TYPE decimals,
      END OF ty_override,
      ty_dd03p_tab TYPE STANDARD TABLE OF dd03p WITH DEFAULT KEY,
      ty_dd08v_tab TYPE STANDARD TABLE OF dd08v WITH DEFAULT KEY.

    DATA:
      ms_file      TYPE ty_def,
      ms_live      TYPE ty_def,
      mt_rows      TYPE string_table,
      mv_ddname    TYPE ddobjname,
      mv_devclass  TYPE devclass,
      mv_local     TYPE abap_bool,
      mv_live_hash TYPE string.

    METHODS do_read.

    METHODS do_deploy.

    METHODS do_create.

    METHODS do_delete.

    METHODS get_def
      IMPORTING
        iv_type  TYPE string
        iv_name  TYPE string
      EXPORTING
        es_def   TYPE ty_def
        ev_found TYPE abap_bool.

    METHODS canonicalize
      CHANGING
        cs_def TYPE ty_def.

    METHODS serialize
      IMPORTING
        is_def         TYPE ty_def
      RETURNING
        VALUE(rt_rows) TYPE string_table.

    METHODS pretty
      IMPORTING
        iv_xml          TYPE string
      RETURNING
        VALUE(rt_lines) TYPE string_table.

    METHODS rows_hash
      IMPORTING
        it_rows        TYPE string_table
      RETURNING
        VALUE(rv_hash) TYPE string.

    METHODS normalize_rows
      CHANGING
        ct_rows TYPE string_table.

    METHODS read_live
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS emit_rows
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS parse_file
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS check_dtel_texts
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS check_tabclass
      IMPORTING
        iv_class     TYPE dd02l-tabclass
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS live_class
      RETURNING
        VALUE(rv_class) TYPE dd02l-tabclass.

    METHODS exists_object
      IMPORTING
        iv_shared        TYPE abap_bool
      RETURNING
        VALUE(rv_exists) TYPE abap_bool.

    METHODS object_package
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS create_package
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS count_inactive
      IMPORTING
        iv_type         TYPE csequence
        iv_name         TYPE csequence
      RETURNING
        VALUE(rv_count) TYPE i.

    METHODS count_rows
      IMPORTING
        iv_tabname     TYPE ddobjname
      RETURNING
        VALUE(rv_rows) TYPE i.

    METHODS is_transparent
      RETURNING
        VALUE(rv_transp) TYPE abap_bool.

    METHODS tabclass_changed
      RETURNING
        VALUE(rv_changed) TYPE abap_bool.

    METHODS check_changes
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS decide_table
      EXPORTING
        ev_action   TYPE tbatg-fct
        ev_modeflag TYPE ddxtt-modeflag
        ev_daction  TYPE string
        ev_allowed  TYPE abap_bool
        ev_rows     TYPE i
        ev_ok       TYPE abap_bool.

    METHODS decide_core
      IMPORTING
        iv_tabname   TYPE ddobjname
        is_dd02v     TYPE dd02v
        is_dd09l     TYPE dd09v
        it_dd03p     TYPE ty_dd03p_tab
        it_dd08v     TYPE ty_dd08v_tab
        is_override  TYPE ty_override OPTIONAL
      EXPORTING
        ev_action    TYPE tbatg-fct
        ev_modeflag  TYPE ddxtt-modeflag
        ev_daction   TYPE string
        ev_allowed   TYPE abap_bool
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS expanded
      IMPORTING
        is_dd02v     TYPE dd02v
      CHANGING
        ct_dd03p     TYPE ty_dd03p_tab
        ct_dd08v     TYPE ty_dd08v_tab
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS key_fields
      IMPORTING
        it_dd03p       TYPE ty_dd03p_tab
      RETURNING
        VALUE(rv_keys) TYPE string.

    METHODS action_allowed
      IMPORTING
        iv_action    TYPE tbatg-fct
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS fill_types
      CHANGING
        ct_dd03p TYPE ty_dd03p_tab.

    METHODS check_dependents
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS decide_dependent
      IMPORTING
        iv_tabname   TYPE ddobjname
        is_override  TYPE ty_override
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS type_of_active
      EXPORTING
        ev_datatype TYPE datatype_d
        ev_leng     TYPE ddleng
        ev_decimals TYPE decimals.

    METHODS users
      RETURNING
        VALUE(rt_users) TYPE ty_users.

    METHODS add_users
      IMPORTING
        iv_object TYPE trobjtype
        iv_name   TYPE ddobjname
      CHANGING
        ct_users  TYPE ty_users.

    METHODS emit_guards.

    METHODS emit_where_used.

    METHODS write_definition
      IMPORTING
        iv_create TYPE abap_bool.

    METHODS put_definition
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS activate_definition
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS verify_definition
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS subset_struct
      IMPORTING
        is_file   TYPE any
        is_active TYPE any
      EXPORTING
        ev_ok     TYPE abap_bool
        ev_diff   TYPE string.

    METHODS subset_table
      IMPORTING
        it_file   TYPE INDEX TABLE
        it_active TYPE INDEX TABLE
      EXPORTING
        ev_ok     TYPE abap_bool
        ev_diff   TYPE string.

    METHODS check_delete
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS delete_request
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS delete_definition.
