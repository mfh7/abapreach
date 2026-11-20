
  METHOD run_type.
    CASE mv_mode.
      WHEN `RUN_PROG`.
        run_prog( ).
      WHEN OTHERS.
        reject( `This runner has no template for the requested mode` ).
    ENDCASE.
  ENDMETHOD.


  METHOD run_prog.
    " Everything that can refuse runs before SUBMIT. After it the report has run and the outcome
    " is RAN. An exception that reaches this method is caught here and rolls the work back.
    TRY.
        IF check_report( ) = abap_false.
          RETURN.
        ENDIF.
        IF build_selection( ) = abap_false.
          RETURN.
        ENDIF.
        IF mv_alv = `on`.
          cl_salv_bs_runtime_info=>set( display = abap_false metadata = abap_true data = abap_true ).
        ENDIF.
        " SUBMIT takes a character-like name, so the string values are converted first.
        DATA(lv_report) = CONV progname( mv_report ).
        DATA(lv_variant) = CONV raldb_vari( mv_variant ).
        TRY.
            IF mv_variant IS INITIAL.
              SUBMIT (lv_report) WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.
            ELSE.
              SUBMIT (lv_report) USING SELECTION-SET lv_variant WITH SELECTION-TABLE mt_sel EXPORTING LIST TO MEMORY AND RETURN.
            ENDIF.
            capture_alv( ).
          CLEANUP.
            cl_salv_bs_runtime_info=>clear_all( ).
        ENDTRY.
        cl_salv_bs_runtime_info=>clear_all( ).
        print_list( ).
      CATCH cx_root INTO DATA(lx_error).
        run_failed( lx_error ).
        RETURN.
    ENDTRY.
    cut_note( ).
    finish( `RAN` ).
  ENDMETHOD.


  METHOD check_report.
    " The checks of SA38, in its order: the report exists and is active, it is executable, the
    " user may submit it, and its active source is the one the simulate bound.
    DATA:
      lt_source TYPE string_table.

    DATA:
      lv_hash TYPE string.

    DATA(lv_name) = CONV progname( mv_report ).
    SELECT SINGLE subc, secu FROM trdir WHERE name = @lv_name INTO @DATA(ls_dir).
    IF sy-subrc <> 0.
      reject( |report { mv_report } does not exist| ).
      RETURN.
    ENDIF.
    READ REPORT lv_name INTO lt_source STATE 'A'.
    IF sy-subrc <> 0.
      reject( |report { mv_report } has no active version| ).
      RETURN.
    ENDIF.
    IF ls_dir-subc <> '1'.
      reject( |report { mv_report } is not an executable program (SUBC={ ls_dir-subc })| ).
      RETURN.
    ENDIF.
    AUTHORITY-CHECK OBJECT 'S_PROGRAM'
      ID 'P_ACTION' FIELD 'SUBMIT'
      ID 'P_GROUP' FIELD ls_dir-secu.
    IF sy-subrc <> 0.
      reject( |not authorized to submit report { mv_report } (S_PROGRAM)| ).
      RETURN.
    ENDIF.
    TRY.
        lv_hash = source_hash( lt_source ).
      CATCH cx_abap_message_digest.
        reject( `report source hash could not be computed` ).
        RETURN.
    ENDTRY.
    IF lv_hash <> mv_base.
      reject( `report changed since the simulate` ).
      RETURN.
    ENDIF.
    IF mv_variant IS NOT INITIAL.
      DATA(lv_variant) = CONV variant( mv_variant ).
      SELECT SINGLE variant FROM varid WHERE report = @lv_name AND variant = @lv_variant INTO @DATA(lv_found).
      IF sy-subrc <> 0.
        reject( |variant { mv_variant } does not exist for report { mv_report }| ).
        RETURN.
      ENDIF.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD source_hash.
    " Lower-case SHA-256 of the active source: no trailing blanks on a row, no empty rows at the
    " end, rows joined by line feeds, as UTF-8. This is the source hash of the client.
    DATA:
      lo_conv TYPE REF TO cl_abap_conv_out_ce.

    DATA:
      lt_rows TYPE string_table.

    DATA:
      lv_x     TYPE xstring,
      lv_text  TYPE string,
      lv_hash  TYPE string,
      lv_index TYPE i.

    lt_rows = it_source.
    LOOP AT lt_rows ASSIGNING FIELD-SYMBOL(<lv_row>).
      <lv_row> = replace( val = <lv_row> regex = ` +$` with = `` ).
    ENDLOOP.
    lv_index = lines( lt_rows ).
    WHILE lv_index > 0.
      IF lt_rows[ lv_index ] IS NOT INITIAL.
        EXIT.
      ENDIF.
      DELETE lt_rows INDEX lv_index.
      lv_index = lv_index - 1.
    ENDWHILE.
    lv_text = concat_lines_of( table = lt_rows sep = |\n| ).
    lo_conv = cl_abap_conv_out_ce=>create( encoding = 'UTF-8' ).
    lo_conv->convert( EXPORTING data = lv_text IMPORTING buffer = lv_x ).
    cl_abap_message_digest=>calculate_hash_for_raw(
      EXPORTING
        if_algorithm  = 'SHA256'
        if_data       = lv_x
      IMPORTING
        ef_hashstring = lv_hash ).
    rv_hash = to_lower( lv_hash ).
  ENDMETHOD.


  METHOD selection_names.
    " LIVE PROBE: RS_REPORTSELECTIONS_INFO is the proposed function module. Its table parameter is
    " created from the type its own interface names, so no structure name is assumed. The probe
    " confirms the parameter REPORT and the NAME and KIND columns. Fallback when the interface
    " differs: no name is known, and a run with parameters stops with a reason.
    DATA:
      lt_ptab TYPE abap_func_parmbind_tab,
      lt_etab TYPE abap_func_excpbind_tab.

    DATA:
      lr_info TYPE REF TO data.

    DATA:
      lv_report TYPE c LENGTH 40.

    FIELD-SYMBOLS:
      <lt_info> TYPE STANDARD TABLE.

    SELECT SINGLE parameter, structure FROM fupararef
      WHERE funcname = 'RS_REPORTSELECTIONS_INFO' AND paramtype = 'T' AND r3state = 'A'
      INTO @DATA(ls_interface).
    IF sy-subrc <> 0 OR ls_interface-structure IS INITIAL.
      RETURN.
    ENDIF.
    lv_report = mv_report.
    TRY.
        CREATE DATA lr_info TYPE STANDARD TABLE OF (ls_interface-structure).
        lt_ptab = VALUE #( ( name = 'REPORT' kind = abap_func_exporting value = REF #( lv_report ) )
                           ( name = ls_interface-parameter kind = abap_func_tables value = lr_info ) ).
        lt_etab = VALUE #( ( name = 'OTHERS' value = 1 ) ).
        CALL FUNCTION 'RS_REPORTSELECTIONS_INFO'
          PARAMETER-TABLE lt_ptab
          EXCEPTION-TABLE lt_etab.
        IF sy-subrc <> 0.
          RETURN.
        ENDIF.
        ASSIGN lr_info->* TO <lt_info>.
        LOOP AT <lt_info> ASSIGNING FIELD-SYMBOL(<ls_info>).
          ASSIGN COMPONENT `NAME` OF STRUCTURE <ls_info> TO FIELD-SYMBOL(<lv_name>).
          IF sy-subrc <> 0.
            CONTINUE.
          ENDIF.
          DATA(lv_kind) = `P`.
          ASSIGN COMPONENT `KIND` OF STRUCTURE <ls_info> TO FIELD-SYMBOL(<lv_kind>).
          IF sy-subrc = 0.
            lv_kind = |{ <lv_kind> }|.
          ENDIF.
          APPEND VALUE #( name = to_upper( |{ <lv_name> }| ) kind = lv_kind ) TO rt_known.
        ENDLOOP.
      CATCH cx_root.
        CLEAR rt_known.
    ENDTRY.
  ENDMETHOD.


  METHOD build_selection.
    " A parameter is a PARAMETERS row; a select-option takes its single value as an I/EQ row.
    IF mt_params IS INITIAL.
      rv_ok = abap_true.
      RETURN.
    ENDIF.
    DATA(lt_known) = selection_names( ).
    IF lt_known IS INITIAL.
      reject( |the selection screen of report { mv_report } cannot be read| ).
      RETURN.
    ENDIF.
    LOOP AT mt_params INTO DATA(lv_param).
      DATA(lv_name) = substring( val = lv_param len = find( val = lv_param sub = `=` ) ).
      DATA(lv_value) = substring_after( val = lv_param sub = `=` ).
      READ TABLE lt_known INTO DATA(ls_known) WITH KEY name = lv_name.
      IF sy-subrc <> 0.
        reject( |parameter { lv_name } is not on the selection screen of report { mv_report }| ).
        RETURN.
      ENDIF.
      IF ls_known-kind = `S`.
        APPEND VALUE #( selname = lv_name kind = 'S' sign = 'I' option = 'EQ' low = lv_value ) TO mt_sel.
      ELSE.
        APPEND VALUE #( selname = lv_name kind = 'P' low = lv_value ) TO mt_sel.
      ENDIF.
    ENDLOOP.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD capture_alv.
    " LIVE PROBE: the interface of get_data_ref and get_metadata, and which grid is captured when
    " a report shows more than one. No grid is a normal run with the list only.
    DATA:
      lr_data TYPE REF TO data.

    FIELD-SYMBOLS:
      <lt_data> TYPE ANY TABLE.

    IF mv_alv <> `on`.
      RETURN.
    ENDIF.
    TRY.
        cl_salv_bs_runtime_info=>get_data_ref( IMPORTING r_data = lr_data ).
      CATCH cx_salv_bs_sc_runtime_info.
        RETURN.
    ENDTRY.
    IF lr_data IS NOT BOUND.
      RETURN.
    ENDIF.
    ASSIGN lr_data->* TO <lt_data>.
    IF sy-subrc <> 0.
      RETURN.
    ENDIF.
    TRY.
        TRY.
            find_columns( cl_salv_bs_runtime_info=>get_metadata( ) ).
          CATCH cx_root.
            CLEAR mt_cols.
        ENDTRY.
        IF mt_cols IS INITIAL.
          LOOP AT <lt_data> ASSIGNING FIELD-SYMBOL(<ls_first>).
            columns_from_row( <ls_first> ).
            EXIT.
          ENDLOOP.
        ENDIF.
        IF mt_cols IS INITIAL.
          RETURN.
        ENDIF.
        emit_columns( ).
        LOOP AT <lt_data> ASSIGNING FIELD-SYMBOL(<ls_row>).
          IF emit_row( <ls_row> ) = abap_false.
            EXIT.
          ENDIF.
        ENDLOOP.
      CATCH cx_root.
        msg( iv_level = `W` iv_text = `The ALV data could not be read; only the list is printed` ).
    ENDTRY.
  ENDMETHOD.


  METHOD find_columns.
    " The field catalog is the table of the metadata whose rows have a FIELDNAME, so no component
    " name of the metadata is assumed.
    FIELD-SYMBOLS:
      <lt_fcat> TYPE ANY TABLE.

    DATA(lo_struct) = CAST cl_abap_structdescr( cl_abap_typedescr=>describe_by_data( is_meta ) ).
    LOOP AT lo_struct->components ASSIGNING FIELD-SYMBOL(<ls_comp>)
         WHERE type_kind = cl_abap_typedescr=>typekind_table.
      ASSIGN COMPONENT <ls_comp>-name OF STRUCTURE is_meta TO <lt_fcat>.
      IF sy-subrc <> 0.
        CONTINUE.
      ENDIF.
      LOOP AT <lt_fcat> ASSIGNING FIELD-SYMBOL(<ls_fc>).
        ASSIGN COMPONENT `FIELDNAME` OF STRUCTURE <ls_fc> TO FIELD-SYMBOL(<lv_name>).
        IF sy-subrc <> 0.
          EXIT.
        ENDIF.
        DATA(lv_name) = |{ <lv_name> }|.
        DATA(lv_text) = comp_text( is_row = <ls_fc> iv_name = `COLTEXT` ).
        IF lv_text IS INITIAL.
          lv_text = comp_text( is_row = <ls_fc> iv_name = `SCRTEXT_L` ).
        ENDIF.
        IF lv_text IS INITIAL.
          lv_text = comp_text( is_row = <ls_fc> iv_name = `REPTEXT` ).
        ENDIF.
        IF lv_text IS INITIAL.
          lv_text = comp_text( is_row = <ls_fc> iv_name = `SELTEXT` ).
        ENDIF.
        IF lv_text IS INITIAL.
          lv_text = lv_name.
        ENDIF.
        APPEND VALUE #(
          name    = lv_name
          text    = lv_text
          visible = COND #( WHEN comp_text( is_row = <ls_fc> iv_name = `TECH` ) = `X`
                              OR comp_text( is_row = <ls_fc> iv_name = `NO_OUT` ) = `X`
                            THEN abap_false
                            ELSE abap_true )
          cfield  = comp_text( is_row = <ls_fc> iv_name = `CFIELDNAME` )
          qfield  = comp_text( is_row = <ls_fc> iv_name = `QFIELDNAME` )
          cfixed  = comp_text( is_row = <ls_fc> iv_name = `CURRENCY` )
          qfixed  = comp_text( is_row = <ls_fc> iv_name = `QUANTITY` ) ) TO mt_cols.
      ENDLOOP.
      IF mt_cols IS NOT INITIAL.
        RETURN.
      ENDIF.
    ENDLOOP.
  ENDMETHOD.


  METHOD columns_from_row.
    " Fallback without a field catalog: every component of the first row, named by itself.
    DATA(lo_type) = cl_abap_typedescr=>describe_by_data( is_row ).
    IF lo_type->kind <> cl_abap_typedescr=>kind_struct.
      RETURN.
    ENDIF.
    DATA(lo_struct) = CAST cl_abap_structdescr( lo_type ).
    LOOP AT lo_struct->components ASSIGNING FIELD-SYMBOL(<ls_comp>).
      APPEND VALUE #( name = |{ <ls_comp>-name }| text = |{ <ls_comp>-name }| visible = abap_true ) TO mt_cols.
    ENDLOOP.
  ENDMETHOD.


  METHOD emit_columns.
    LOOP AT mt_cols INTO DATA(ls_col).
      DATA(lv_flag) = COND string( WHEN ls_col-visible = abap_true THEN `1` ELSE `0` ).
      emit( |ALVCOL:{ lv_flag }:{ hex_of( ls_col-name ) }:{ hex_of( ls_col-text ) }| ).
    ENDLOOP.
  ENDMETHOD.


  METHOD emit_row.
    " One row: every column of the catalog, so the caller can show the visible ones or all.
    DATA:
      lt_cells TYPE string_table.

    LOOP AT mt_cols INTO DATA(ls_col).
      APPEND hex_of( cell_text( is_row = is_row is_col = ls_col ) ) TO lt_cells.
    ENDLOOP.
    emit_budgeted( |ALVROW:{ concat_lines_of( table = lt_cells sep = `,` ) }| ).
    rv_ok = COND #( WHEN mv_cut = abap_true THEN abap_false ELSE abap_true ).
  ENDMETHOD.


  METHOD cell_text.
    " An amount is written with the currency the catalog names and a quantity with its unit, so the
    " decimals match SA38 (JPY has none). Any other value is its internal value, with a point as
    " the decimal separator. A cell that is not elementary is empty.
    DATA:
      lv_out   TYPE c LENGTH 60,
      lv_waers TYPE waers,
      lv_unit  TYPE meins.

    ASSIGN COMPONENT is_col-name OF STRUCTURE is_row TO FIELD-SYMBOL(<lv_cell>).
    IF sy-subrc <> 0.
      RETURN.
    ENDIF.
    DATA(lo_type) = cl_abap_typedescr=>describe_by_data( <lv_cell> ).
    IF lo_type->kind <> cl_abap_typedescr=>kind_elem.
      RETURN.
    ENDIF.
    DATA(lv_kind) = abap_false.
    IF lo_type->type_kind = cl_abap_typedescr=>typekind_packed.
      IF is_col-cfield IS NOT INITIAL.
        ASSIGN COMPONENT is_col-cfield OF STRUCTURE is_row TO FIELD-SYMBOL(<lv_cur>).
        IF sy-subrc = 0.
          lv_waers = <lv_cur>.
          lv_kind = abap_true.
        ENDIF.
      ELSEIF is_col-cfixed IS NOT INITIAL.
        lv_waers = is_col-cfixed.
        lv_kind = abap_true.
      ENDIF.
      IF lv_kind = abap_true.
        WRITE <lv_cell> TO lv_out CURRENCY lv_waers NO-GROUPING LEFT-JUSTIFIED.
      ELSE.
        IF is_col-qfield IS NOT INITIAL.
          ASSIGN COMPONENT is_col-qfield OF STRUCTURE is_row TO FIELD-SYMBOL(<lv_qty>).
          IF sy-subrc = 0.
            lv_unit = <lv_qty>.
            lv_kind = abap_true.
          ENDIF.
        ELSEIF is_col-qfixed IS NOT INITIAL.
          lv_unit = is_col-qfixed.
          lv_kind = abap_true.
        ENDIF.
        IF lv_kind = abap_true.
          WRITE <lv_cell> TO lv_out UNIT lv_unit NO-GROUPING LEFT-JUSTIFIED.
        ENDIF.
      ENDIF.
    ENDIF.
    IF lv_kind = abap_false.
      rv_text = |{ <lv_cell> }|.
      RETURN.
    ENDIF.
    rv_text = condense( lv_out ).
    IF strlen( rv_text ) > 0.
      " WRITE puts the sign behind the number.
      IF substring( val = rv_text off = strlen( rv_text ) - 1 len = 1 ) = `-`.
        rv_text = `-` && substring( val = rv_text len = strlen( rv_text ) - 1 ).
      ENDIF.
    ENDIF.
  ENDMETHOD.


  METHOD comp_text.
    ASSIGN COMPONENT iv_name OF STRUCTURE is_row TO FIELD-SYMBOL(<lv_value>).
    IF sy-subrc = 0.
      rv_text = |{ <lv_value> }|.
    ENDIF.
  ENDMETHOD.


  METHOD print_list.
    " The list the report wrote to memory, as text lines. The memory is always freed.
    DATA:
      lt_list  TYPE STANDARD TABLE OF abaplist WITH DEFAULT KEY,
      lt_lines TYPE list_string_table.

    CALL FUNCTION 'LIST_FROM_MEMORY'
      TABLES
        listobject = lt_list
      EXCEPTIONS
        not_found  = 1
        OTHERS     = 2.
    IF sy-subrc <> 0.
      RETURN.
    ENDIF.
    CALL FUNCTION 'LIST_TO_ASCI'
      IMPORTING
        list_string_ascii  = lt_lines
      TABLES
        listobject         = lt_list
      EXCEPTIONS
        empty_list         = 1
        list_index_invalid = 2
        OTHERS             = 3.
    IF sy-subrc = 0.
      LOOP AT lt_lines INTO DATA(lv_line).
        data_line( lv_line ).
      ENDLOOP.
    ENDIF.
    CALL FUNCTION 'LIST_FREE_MEMORY'
      TABLES
        listobject = lt_list.
  ENDMETHOD.
