  PUBLIC
  FINAL
  CREATE PUBLIC.

  PUBLIC SECTION.
    INTERFACES if_oo_adt_classrun.

  PRIVATE SECTION.
    TYPES:
      BEGIN OF ty_pair,
        pname  TYPE string,
        pvalue TYPE string,
      END OF ty_pair,
      ty_pairs TYPE STANDARD TABLE OF ty_pair WITH DEFAULT KEY,
      BEGIN OF ty_check,
        label  TYPE string,
        cvalue TYPE string,
        regex  TYPE string,
      END OF ty_check,
      ty_checks TYPE STANDARD TABLE OF ty_check WITH DEFAULT KEY.

    DATA:
      mo_out     TYPE REF TO if_oo_adt_intrnl_classrun,
      mt_pairs   TYPE ty_pairs,
      mt_xml     TYPE string_table,
      mt_batch   TYPE string_table,
      mv_nonce   TYPE string,
      mv_mode    TYPE string,
      mv_type    TYPE string,
      mv_name    TYPE string,
      mv_package TYPE string,
      mv_tr      TYPE string,
      mv_base    TYPE string,
      mv_plan    TYPE string,
      mv_report  TYPE string,
      mv_text    TYPE string,
      mv_count   TYPE i,
      mv_done    TYPE abap_bool,
      mv_locked  TYPE abap_bool.

    METHODS emit
      IMPORTING
        iv_body TYPE string.

    METHODS hex_of
      IMPORTING
        iv_text       TYPE string
      RETURNING
        VALUE(rv_hex) TYPE string.

    METHODS msg
      IMPORTING
        iv_level TYPE string
        iv_text  TYPE string.

    METHODS data_line
      IMPORTING
        iv_text TYPE string.

    METHODS fact
      IMPORTING
        iv_key  TYPE string
        iv_text TYPE string.

    METHODS partial_delete
      IMPORTING
        iv_text TYPE string.

    METHODS still_exists
      IMPORTING
        iv_text TYPE string.

    METHODS finish
      IMPORTING
        iv_status TYPE string.

    METHODS reject
      IMPORTING
        iv_text TYPE string.

    METHODS payload
      RETURNING
        VALUE(rv_b64) TYPE string.

    METHODS load_payload
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS value_of
      IMPORTING
        iv_name         TYPE string
      RETURNING
        VALUE(rv_value) TYPE string.

    METHODS check_values
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS guard_system
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS check_transport
      IMPORTING
        iv_devclass  TYPE string
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS check_base
      IMPORTING
        iv_live_hash TYPE string
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS check_inactive
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS lock_object
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS free_object.

    METHODS record_object
      IMPORTING
        iv_devclass  TYPE string
        iv_mode      TYPE string
      RETURNING
        VALUE(rv_ok) TYPE abap_bool.

    METHODS sy_text
      RETURNING
        VALUE(rv_text) TYPE string.

    METHODS run_type.
