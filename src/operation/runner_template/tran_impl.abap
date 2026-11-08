
  METHOD run_type.
    IF mv_type <> `TRAN`.
      reject( `This runner has no template for the requested type` ).
      RETURN.
    ENDIF.
    mv_tcode = mv_name.
    mv_objname = mv_name.
    CASE mv_mode.
      WHEN `READ`.
        tran_read( ).
      WHEN `CREATE_SIM` OR `CREATE`.
        tran_create( ).
      WHEN `DELETE_SIM` OR `DELETE`.
        tran_delete( ).
      WHEN OTHERS.
        reject( |A transaction has no { mv_mode } mode| ).
    ENDCASE.
  ENDMETHOD.


  METHOD tran_live.
    " The live definition as six lines, read from the tables like the Python tool. The type of
    " the transaction is not checked.
    CLEAR: ms_tran, mt_live.
    SELECT SINGLE pgmna, dypno, cinfo FROM tstc
      WHERE tcode = @mv_tcode
      INTO ( @ms_tran-program, @ms_tran-dynpro, @ms_tran-cinfo ).
    IF sy-subrc <> 0.
      RETURN.
    ENDIF.
    SELECT SINGLE ttext FROM tstct
      WHERE sprsl = @sy-langu AND tcode = @mv_tcode
      INTO @ms_tran-text.
    SELECT SINGLE devclass, author FROM tadir
      WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname
      INTO ( @ms_tran-devclass, @ms_tran-author ).
    mt_live = VALUE #( ( |TRAN { mv_tcode }| )
                       ( |program={ ms_tran-program }| )
                       ( |dynpro={ ms_tran-dynpro }| )
                       ( |cinfo={ ms_tran-cinfo }| )
                       ( |text={ ms_tran-text }| )
                       ( |package={ ms_tran-devclass }| ) ).
    rv_found = abap_true.
  ENDMETHOD.


  METHOD tran_hash.
    " Lower-case SHA-256 of the lines without trailing blanks, joined by line feeds, as UTF-8.
    DATA:
      lo_conv TYPE REF TO cl_abap_conv_out_ce.

    DATA:
      lt_lines TYPE string_table.

    DATA:
      lv_x    TYPE xstring,
      lv_text TYPE string,
      lv_hash TYPE string.

    lt_lines = mt_live.
    LOOP AT lt_lines ASSIGNING FIELD-SYMBOL(<lv_line>).
      <lv_line> = shift_right( val = <lv_line> sub = ` ` ).
    ENDLOOP.
    lv_text = concat_lines_of( table = lt_lines sep = |\n| ).
    TRY.
        lo_conv = cl_abap_conv_out_ce=>create( encoding = 'UTF-8' ).
        lo_conv->convert( EXPORTING data = lv_text IMPORTING buffer = lv_x ).
        cl_abap_message_digest=>calculate_hash_for_raw(
          EXPORTING
            if_algorithm  = 'SHA256'
            if_data       = lv_x
          IMPORTING
            ef_hashstring = lv_hash ).
      CATCH cx_root.
        RETURN.
    ENDTRY.
    rv_hash = to_lower( lv_hash ).
  ENDMETHOD.


  METHOD tran_read.
    IF tran_live( ) = abap_false.
      msg( iv_level = `E` iv_text = |Transaction { mv_name } does not exist (TSTC)| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    LOOP AT mt_live INTO DATA(lv_line).
      data_line( lv_line ).
    ENDLOOP.
    finish( `OK` ).
  ENDMETHOD.


  METHOD tran_create.
    " The same checks run in simulate and in the real run; only the real run writes.
    DATA:
      lv_package TYPE devclass,
      lv_report  TYPE trdir-name,
      lv_subc    TYPE trdir-subc,
      lv_first   TYPE string,
      lv_count   TYPE i.

    lv_first = substring( val = mv_name len = 1 ).
    IF lv_first <> `Z` AND lv_first <> `Y`.
      reject( `Only customer names can be created` ).
      RETURN.
    ENDIF.
    " A row with DELFLAG X (deleted, delete not transported) does not hold the name.
    SELECT COUNT(*) FROM tstc WHERE tcode = @mv_tcode INTO @lv_count.
    IF lv_count = 0.
      SELECT COUNT(*) FROM tadir
        WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname AND delflag = @space
        INTO @lv_count.
    ENDIF.
    IF lv_count > 0.
      reject( |Object TRAN { mv_name } already exists (TSTC/TADIR)| ).
      RETURN.
    ENDIF.
    IF mv_package IS INITIAL.
      mv_package = `$TMP`.
    ENDIF.
    lv_package = mv_package.
    SELECT COUNT(*) FROM tdevc WHERE devclass = @lv_package INTO @lv_count.
    IF lv_count = 0.
      reject( |Package { mv_package } does not exist (TDEVC)| ).
      RETURN.
    ENDIF.
    lv_first = substring( val = mv_package len = 1 ).
    IF lv_first <> `$` AND lv_first <> `Z` AND lv_first <> `Y`.
      reject( |Package { mv_package } is not a local or customer package| ).
      RETURN.
    ENDIF.
    IF mv_text IS INITIAL.
      reject( `A transaction text is required` ).
      RETURN.
    ENDIF.
    lv_report = mv_report.
    SELECT SINGLE subc FROM trdir WHERE name = @lv_report INTO @lv_subc.
    IF sy-subrc <> 0 OR lv_subc <> '1'.
      reject( |A transaction needs exactly one existing executable report (SUBC 1): { mv_report }| ).
      RETURN.
    ENDIF.
    lv_first = substring( val = mv_report len = 1 ).
    IF lv_first <> `Z` AND lv_first <> `Y`.
      reject( |Report { mv_report } is not a customer report| ).
      RETURN.
    ENDIF.
    IF check_transport( mv_package ) = abap_false.
      RETURN.
    ENDIF.
    IF mv_mode = `CREATE_SIM`.
      finish( `SIMULATED_OK` ).
      RETURN.
    ENDIF.
    tran_insert( ).
  ENDMETHOD.


  METHOD tran_insert.
    " The recording writes the TADIR row and takes the global lock; the insert then writes TSTC.
    " Without TSTC the recorded name would stay held, so a failed insert undoes the TADIR row.
    DATA:
      lv_trkorr  TYPE trkorr,
      lv_package TYPE devclass,
      lv_report  TYPE trdir-name,
      lv_rc      TYPE sysubrc,
      lv_count   TYPE i,
      lv_text    TYPE string.

    SELECT COUNT(*) FROM tadir
      WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname
      INTO @lv_count.
    DATA(lv_had_row) = xsdbool( lv_count > 0 ).
    IF record_object( iv_devclass = mv_package iv_mode = `I` ) = abap_false.
      RETURN.
    ENDIF.
    lv_trkorr = mv_tr.
    lv_package = mv_package.
    lv_report = mv_report.
    CALL FUNCTION 'RPY_TRANSACTION_INSERT'
      EXPORTING
        transaction          = mv_tcode
        program              = lv_report
        dynpro               = '1000'
        language             = sy-langu
        development_class    = lv_package
        transport_number     = lv_trkorr
        transaction_type     = 'R'
        shorttext            = CONV tstct-ttext( mv_text )
        html_enabled         = 'X'
        java_enabled         = 'X'
        wingui_enabled       = 'X'
        suppress_corr_insert = 'X'
      EXCEPTIONS
        cancelled            = 1
        already_exist        = 2
        permission_error     = 3
        name_not_allowed     = 4
        name_conflict        = 5
        illegal_type         = 6
        object_inconsistent  = 7
        db_access_error      = 8
        error_message        = 9
        OTHERS               = 10.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      lv_text = sy_text( ).
      msg( iv_level = `E` iv_text = |RPY_TRANSACTION_INSERT failed (rc={ lv_rc }): { lv_text }| ).
      tran_undo( lv_had_row ).
      RETURN.
    ENDIF.
    COMMIT WORK AND WAIT.
    " In-run proof: TSTC points to the report and TADIR holds the package.
    SELECT SINGLE pgmna FROM tstc WHERE tcode = @mv_tcode INTO @DATA(lv_pgmna).
    SELECT SINGLE devclass FROM tadir
      WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname
      INTO @DATA(lv_dev).
    IF lv_pgmna <> mv_report OR lv_dev <> mv_package.
      msg( iv_level = `E` iv_text = |TRAN { mv_name } incomplete: TSTC-PGMNA={ lv_pgmna } TADIR={ lv_dev }| ).
      finish( `INACTIVE` ).
      RETURN.
    ENDIF.
    finish( `ACTIVE` ).
  ENDMETHOD.


  METHOD tran_undo.
    " A row that existed before the recording (DELFLAG X) gets the flag back; otherwise the row
    " the recording wrote is removed. A row that stays is reported, and the status is PARTIAL.
    DATA:
      lv_count TYPE i.

    IF iv_had_row = abap_true.
      CALL FUNCTION 'TR_TADIR_INTERFACE'
        EXPORTING
          wi_test_modus     = space
          wi_tadir_pgmid    = 'R3TR'
          wi_tadir_object   = 'TRAN'
          wi_tadir_obj_name = CONV sobj_name( mv_name )
          iv_delflag        = 'X'
        EXCEPTIONS
          OTHERS            = 1.
    ELSE.
      CALL FUNCTION 'TR_TADIR_INTERFACE'
        EXPORTING
          wi_delete_tadir_entry = 'X'
          wi_test_modus         = space
          wi_tadir_pgmid        = 'R3TR'
          wi_tadir_object       = 'TRAN'
          wi_tadir_obj_name     = CONV sobj_name( mv_name )
        EXCEPTIONS
          OTHERS                = 1.
    ENDIF.
    COMMIT WORK AND WAIT.
    SELECT COUNT(*) FROM tadir
      WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname AND delflag = @space
      INTO @lv_count.
    IF lv_count > 0.
      msg( iv_level = `E` iv_text = |TADIR R3TR TRAN { mv_name } left behind without TSTC| ).
      finish( `PARTIAL` ).
      RETURN.
    ENDIF.
    IF mv_tr IS NOT INITIAL.
      msg( iv_level = `W` iv_text = |TRAN { mv_name } remains recorded in request { mv_tr }| ).
    ENDIF.
    finish( `REJECTED` ).
  ENDMETHOD.


  METHOD tran_delete.
    DATA:
      lv_first TYPE string,
      lv_hash  TYPE string.

    lv_first = substring( val = mv_name len = 1 ).
    IF lv_first <> `Z` AND lv_first <> `Y`.
      reject( `Only customer names can be deleted` ).
      RETURN.
    ENDIF.
    IF tran_live( ) = abap_false.
      msg( iv_level = `E` iv_text = |Object TRAN { mv_name } does not exist (TSTC)| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    IF ms_tran-devclass IS INITIAL.
      msg( iv_level = `E` iv_text = |Object TRAN { mv_name } does not exist (TADIR R3TR TRAN)| ).
      finish( `NOT_FOUND` ).
      RETURN.
    ENDIF.
    IF tran_request( ) = abap_false.
      RETURN.
    ENDIF.
    lv_hash = tran_hash( ).
    IF lv_hash IS INITIAL.
      reject( `The hash of the live definition cannot be computed` ).
      RETURN.
    ENDIF.
    IF mv_mode = `DELETE_SIM`.
      " The plan: package, author, request, program, cinfo, and the hash of the six lines.
      fact( iv_key = `PACKAGE` iv_text = |{ ms_tran-devclass }| ).
      fact( iv_key = `AUTHOR` iv_text = |{ ms_tran-author }| ).
      fact( iv_key = `PROGRAM` iv_text = |{ ms_tran-program }| ).
      fact( iv_key = `CINFO` iv_text = |{ ms_tran-cinfo }| ).
      fact( iv_key = `REQUEST` iv_text = COND #( WHEN mv_tr IS INITIAL THEN `-` ELSE mv_tr ) ).
      fact( iv_key = `HASH` iv_text = lv_hash ).
      finish( `SIMULATED_OK` ).
      RETURN.
    ENDIF.
    IF mv_plan <> lv_hash.
      reject( `Plan changed since simulate; simulate again` ).
      RETURN.
    ENDIF.
    tran_remove( ).
  ENDMETHOD.


  METHOD tran_request.
    " A local object takes no request. An object locked in a modifiable task follows the request
    " of that task when the request is the user's own; another user's request is refused. An
    " object that is not locked needs the request of the payload.
    DATA:
      lv_obj TYPE e071-obj_name.

    IF substring( val = |{ ms_tran-devclass }| len = 1 ) = `$`.
      IF mv_tr IS NOT INITIAL.
        msg( iv_level = `W` iv_text = |Local object { ms_tran-devclass }: request { mv_tr } is ignored| ).
        CLEAR mv_tr.
      ENDIF.
      rv_ok = abap_true.
      RETURN.
    ENDIF.
    lv_obj = mv_name.
    SELECT DISTINCT e070~strkorr FROM e071
      INNER JOIN e070 ON e070~trkorr = e071~trkorr
      WHERE e071~pgmid = 'R3TR' AND e071~object = 'TRAN' AND e071~obj_name = @lv_obj
        AND e070~trstatus = 'D' AND e070~trfunction <> 'T' AND e070~strkorr <> @space
      INTO TABLE @DATA(lt_requests).
    CASE lines( lt_requests ).
      WHEN 0.
        rv_ok = check_transport( |{ ms_tran-devclass }| ).
      WHEN 1.
        DATA(lv_request) = lt_requests[ 1 ]-strkorr.
        SELECT SINGLE trfunction, trstatus, as4user FROM e070
          WHERE trkorr = @lv_request
          INTO @DATA(ls_e070).
        SELECT COUNT(*) FROM e070
          WHERE strkorr = @lv_request AND as4user = @sy-uname AND trstatus = 'D'
          INTO @DATA(lv_own_tasks).
        IF ls_e070-as4user <> sy-uname AND lv_own_tasks = 0.
          reject( |Object TRAN { mv_name } is locked in request { lv_request } of { ls_e070-as4user }| ).
          RETURN.
        ENDIF.
        IF ls_e070-trfunction <> 'K' OR ls_e070-trstatus <> 'D'.
          reject( |Object TRAN { mv_name } is locked in request { lv_request } ({ ls_e070-trfunction }/{ ls_e070-trstatus })| ).
          RETURN.
        ENDIF.
        IF mv_tr IS NOT INITIAL AND mv_tr <> lv_request.
          msg( iv_level = `W` iv_text = |Request { mv_tr } is ignored: the object is locked in { lv_request }| ).
        ENDIF.
        mv_tr = lv_request.
        rv_ok = abap_true.
      WHEN OTHERS.
        reject( |Object TRAN { mv_name } is locked in more than one request| ).
    ENDCASE.
  ENDMETHOD.


  METHOD tran_remove.
    " The delete is recorded in the request first (own or locked), then the transaction goes. A
    " failure after the first change rolls back and is reported as a partial delete.
    DATA:
      lv_trkorr TYPE trkorr,
      lv_rc     TYPE sysubrc,
      lv_count  TYPE i,
      lv_text   TYPE string.

    lv_trkorr = mv_tr.
    IF lv_trkorr IS NOT INITIAL.
      CALL FUNCTION 'RS_CORR_INSERT'
        EXPORTING
          object              = CONV e071-obj_name( mv_name )
          object_class        = 'TRAN'
          mode                = 'D'
          global_lock         = 'X'
          korrnum             = lv_trkorr
          suppress_dialog     = 'X'
        EXCEPTIONS
          cancelled           = 1
          permission_failure  = 2
          unknown_objectclass = 3
          error_message       = 4
          OTHERS              = 5.
      IF sy-subrc <> 0.
        lv_rc = sy-subrc.
        reject( |Recording TRAN { mv_name } for delete failed (rc={ lv_rc }): { sy_text( ) }| ).
        RETURN.
      ENDIF.
    ENDIF.
    CALL FUNCTION 'RPY_TRANSACTION_DELETE'
      EXPORTING
        transaction          = mv_tcode
        transport_number     = lv_trkorr
        suppress_corr_insert = 'X'
      EXCEPTIONS
        not_excecuted        = 1
        object_not_found     = 2
        error_message        = 3
        OTHERS               = 4.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      lv_text = sy_text( ).
      ROLLBACK WORK.
      msg( iv_level = `E` iv_text = |RPY_TRANSACTION_DELETE failed (rc={ lv_rc }): { lv_text }| ).
      partial_delete( `The delete may have partially happened; read the object to check` ).
      finish( `PARTIAL` ).
      RETURN.
    ENDIF.
    " The delete removes a local TADIR row itself; a transported row stays flagged for the import.
    SELECT COUNT(*) FROM tadir
      WHERE pgmid = 'R3TR' AND object = 'TRAN' AND obj_name = @mv_objname AND delflag = @space
      INTO @lv_count.
    IF lv_count > 0.
      IF lv_trkorr IS INITIAL.
        CALL FUNCTION 'TR_TADIR_INTERFACE'
          EXPORTING
            wi_delete_tadir_entry = 'X'
            wi_test_modus         = space
            wi_tadir_pgmid        = 'R3TR'
            wi_tadir_object       = 'TRAN'
            wi_tadir_obj_name     = CONV sobj_name( mv_name )
          EXCEPTIONS
            OTHERS                = 1.
      ELSE.
        CALL FUNCTION 'TR_TADIR_INTERFACE'
          EXPORTING
            wi_test_modus     = space
            wi_tadir_pgmid    = 'R3TR'
            wi_tadir_object   = 'TRAN'
            wi_tadir_obj_name = CONV sobj_name( mv_name )
            iv_delflag        = 'X'
          EXCEPTIONS
            OTHERS            = 1.
      ENDIF.
      IF sy-subrc <> 0.
        lv_rc = sy-subrc.
        msg( iv_level = `W` iv_text = |TADIR R3TR TRAN { mv_name } not updated (rc={ lv_rc })| ).
      ENDIF.
    ENDIF.
    COMMIT WORK AND WAIT.
    SELECT COUNT(*) FROM tstc WHERE tcode = @mv_tcode INTO @lv_count.
    IF lv_count = 0.
      finish( `DELETED` ).
    ELSE.
      still_exists( |Object TRAN { mv_name } still exists after the delete| ).
      finish( `PARTIAL` ).
    ENDIF.
  ENDMETHOD.
