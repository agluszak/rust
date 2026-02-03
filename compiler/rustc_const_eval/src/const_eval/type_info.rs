use rustc_abi::FieldIdx;
use rustc_ast::Mutability;
use rustc_hir::LangItem;
use rustc_middle::span_bug;
use rustc_middle::ty::layout::TyAndLayout;
use rustc_middle::ty::{self, Const, ScalarInt, Ty};
use rustc_span::{Symbol, sym};

use crate::const_eval::CompileTimeMachine;
use crate::interpret::{
    CtfeProvenance, Immediate, InterpCx, InterpResult, MPlaceTy, MemoryKind, Scalar, Writeable,
    interp_ok,
};

impl<'tcx> InterpCx<'tcx, CompileTimeMachine<'tcx>> {
    /// Writes a `core::mem::type_info::TypeInfo` for a given type, `ty` to the given place.
    pub(crate) fn write_type_info(
        &mut self,
        ty: Ty<'tcx>,
        dest: &impl Writeable<'tcx, CtfeProvenance>,
    ) -> InterpResult<'tcx> {
        let ty_struct = self.tcx.require_lang_item(LangItem::Type, self.tcx.span);
        let ty_struct = self.tcx.type_of(ty_struct).no_bound_vars().unwrap();
        assert_eq!(ty_struct, dest.layout().ty);
        let ty_struct = ty_struct.ty_adt_def().unwrap().non_enum_variant();
        // Fill all fields of the `TypeInfo` struct.
        for (idx, field) in ty_struct.fields.iter_enumerated() {
            let field_dest = self.project_field(dest, idx)?;
            let downcast = |name: Symbol| {
                let variants = field_dest.layout().ty.ty_adt_def().unwrap().variants();
                let variant_id = variants
                    .iter_enumerated()
                    .find(|(_idx, var)| var.name == name)
                    .unwrap_or_else(|| panic!("got {name} but expected one of {variants:#?}"))
                    .0;

                interp_ok((variant_id, self.project_downcast(&field_dest, variant_id)?))
            };
            let ptr_bit_width = || self.tcx.data_layout.pointer_size().bits();
            match field.name {
                sym::kind => {
                    let variant_index = match ty.kind() {
                        ty::Tuple(fields) => {
                            let (variant, variant_place) = downcast(sym::Tuple)?;
                            // project to the single tuple variant field of `type_info::Tuple` struct type
                            let tuple_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            assert_eq!(
                                1,
                                tuple_place
                                    .layout()
                                    .ty
                                    .ty_adt_def()
                                    .unwrap()
                                    .non_enum_variant()
                                    .fields
                                    .len()
                            );
                            self.write_tuple_fields(tuple_place, fields, ty)?;
                            variant
                        }
                        ty::Array(ty, len) => {
                            let (variant, variant_place) = downcast(sym::Array)?;
                            let array_place = self.project_field(&variant_place, FieldIdx::ZERO)?;

                            self.write_array_type_info(array_place, *ty, *len)?;

                            variant
                        }
                        ty::Slice(ty) => {
                            let (variant, variant_place) = downcast(sym::Slice)?;
                            let slice_place = self.project_field(&variant_place, FieldIdx::ZERO)?;

                            self.write_slice_type_info(slice_place, *ty)?;

                            variant
                        }
                        ty::Bool => {
                            let (variant, _variant_place) = downcast(sym::Bool)?;
                            variant
                        }
                        ty::Char => {
                            let (variant, _variant_place) = downcast(sym::Char)?;
                            variant
                        }
                        ty::Int(int_ty) => {
                            let (variant, variant_place) = downcast(sym::Int)?;
                            let place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_int_type_info(
                                place,
                                int_ty.bit_width().unwrap_or_else(/* isize */ ptr_bit_width),
                                true,
                            )?;
                            variant
                        }
                        ty::Uint(uint_ty) => {
                            let (variant, variant_place) = downcast(sym::Int)?;
                            let place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_int_type_info(
                                place,
                                uint_ty.bit_width().unwrap_or_else(/* usize */ ptr_bit_width),
                                false,
                            )?;
                            variant
                        }
                        ty::Float(float_ty) => {
                            let (variant, variant_place) = downcast(sym::Float)?;
                            let place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_float_type_info(place, float_ty.bit_width())?;
                            variant
                        }
                        ty::Str => {
                            let (variant, _variant_place) = downcast(sym::Str)?;
                            variant
                        }
                        ty::Ref(_, ty, mutability) => {
                            let (variant, variant_place) = downcast(sym::Reference)?;
                            let reference_place =
                                self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_reference_type_info(reference_place, *ty, *mutability)?;

                            variant
                        }
                        ty::RawPtr(ty, mutability) => {
                            let (variant, variant_place) = downcast(sym::Pointer)?;
                            let pointer_place =
                                self.project_field(&variant_place, FieldIdx::ZERO)?;

                            self.write_pointer_type_info(pointer_place, *ty, *mutability)?;

                            variant
                        }
                        ty::Dynamic(predicates, region) => {
                            let (variant, variant_place) = downcast(sym::DynTrait)?;
                            let dyn_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_dyn_trait_type_info(dyn_place, *predicates, *region)?;
                            variant
                        }
                        ty::Adt(def, args) => {
                            let (variant, variant_place) = downcast(sym::Adt)?;
                            let adt_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                            self.write_adt_type_info(adt_place, ty, def, args)?;
                            variant
                        }
                        ty::Foreign(_)
                        | ty::Pat(_, _)
                        | ty::FnDef(..)
                        | ty::FnPtr(..)
                        | ty::UnsafeBinder(..)
                        | ty::Closure(..)
                        | ty::CoroutineClosure(..)
                        | ty::Coroutine(..)
                        | ty::CoroutineWitness(..)
                        | ty::Never
                        | ty::Alias(..)
                        | ty::Param(_)
                        | ty::Bound(..)
                        | ty::Placeholder(_)
                        | ty::Infer(..)
                        | ty::Error(_) => downcast(sym::Other)?.0,
                    };
                    self.write_discriminant(variant_index, &field_dest)?
                }
                sym::size => {
                    let layout = self.layout_of(ty)?;
                    let variant_index = if layout.is_sized() {
                        let (variant, variant_place) = downcast(sym::Some)?;
                        let size_field_place =
                            self.project_field(&variant_place, FieldIdx::ZERO)?;
                        self.write_scalar(
                            ScalarInt::try_from_target_usize(layout.size.bytes(), self.tcx.tcx)
                                .unwrap(),
                            &size_field_place,
                        )?;
                        variant
                    } else {
                        downcast(sym::None)?.0
                    };
                    self.write_discriminant(variant_index, &field_dest)?;
                }
                other => span_bug!(self.tcx.span, "unknown `Type` field {other}"),
            }
        }

        interp_ok(())
    }

    pub(crate) fn write_tuple_fields(
        &mut self,
        tuple_place: impl Writeable<'tcx, CtfeProvenance>,
        fields: &[Ty<'tcx>],
        tuple_ty: Ty<'tcx>,
    ) -> InterpResult<'tcx> {
        // project into the `type_info::Tuple::fields` field
        let fields_slice_place = self.project_field(&tuple_place, FieldIdx::ZERO)?;
        // get the `type_info::Field` type from `fields: &[Field]`
        let field_type = fields_slice_place
            .layout()
            .ty
            .builtin_deref(false)
            .unwrap()
            .sequence_element_type(self.tcx.tcx);
        // Create an array with as many elements as the number of fields in the inspected tuple
        let fields_layout =
            self.layout_of(Ty::new_array(self.tcx.tcx, field_type, fields.len() as u64))?;
        let fields_place = self.allocate(fields_layout, MemoryKind::Stack)?;
        let mut fields_places = self.project_array_fields(&fields_place)?;

        let tuple_layout = self.layout_of(tuple_ty)?;

        while let Some((i, place)) = fields_places.next(self)? {
            let field_ty = fields[i as usize];
            // For tuples, field name is None
            self.write_field_with_name(None, field_ty, place, tuple_layout, i)?;
        }

        let fields_place = fields_place.map_provenance(CtfeProvenance::as_immutable);

        let ptr = Immediate::new_slice(fields_place.ptr(), fields.len() as u64, self);

        self.write_immediate(ptr, &fields_slice_place)
    }

    fn write_field_with_name(
        &mut self,
        field_name: Option<&str>,
        field_ty: Ty<'tcx>,
        place: MPlaceTy<'tcx>,
        layout: TyAndLayout<'tcx>,
        idx: u64,
    ) -> InterpResult<'tcx> {
        for (field_idx, field_ty_field) in
            place.layout.ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;
            match field_ty_field.name {
                sym::name => {
                    // Write field name as Option<&'static str>
                    if let Some(name) = field_name {
                        // Write Some(name) for named fields
                        let (some_variant, some_place) = self.downcast_option(sym::Some, &field_place)?;
                        let str_place = self.project_field(&some_place, FieldIdx::ZERO)?;
                        self.write_str_slice(&str_place, name)?;
                        self.write_discriminant(some_variant, &field_place)?;
                    } else {
                        // Write None for tuple fields
                        let (none_variant, _) = self.downcast_option(sym::None, &field_place)?;
                        self.write_discriminant(none_variant, &field_place)?;
                    }
                }
                sym::ty => self.write_type_id(field_ty, &field_place)?,
                sym::offset => {
                    let offset = layout.fields.offset(idx as usize);
                    self.write_scalar(
                        ScalarInt::try_from_target_usize(offset.bytes(), self.tcx.tcx).unwrap(),
                        &field_place,
                    )?;
                }
                other => {
                    span_bug!(self.tcx.def_span(field_ty_field.did), "unimplemented field {other}")
                }
                }
                }
            }
        }
        interp_ok(())
    }

    pub(crate) fn write_array_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        len: Const<'tcx>,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Array`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                // Write the `TypeId` of the array's elements to the `element_ty` field.
                sym::element_ty => self.write_type_id(ty, &field_place)?,
                // Write the length of the array to the `len` field.
                sym::len => self.write_scalar(len.to_leaf(), &field_place)?,
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    pub(crate) fn write_slice_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Slice`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                // Write the `TypeId` of the slice's elements to the `element_ty` field.
                sym::element_ty => self.write_type_id(ty, &field_place)?,
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn write_int_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        bit_width: u64,
        signed: bool,
    ) -> InterpResult<'tcx> {
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;
            match field.name {
                sym::bits => self.write_scalar(
                    Scalar::from_u32(bit_width.try_into().expect("bit_width overflowed")),
                    &field_place,
                )?,
                sym::signed => self.write_scalar(Scalar::from_bool(signed), &field_place)?,
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }
        interp_ok(())
    }

    fn write_float_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        bit_width: u64,
    ) -> InterpResult<'tcx> {
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;
            match field.name {
                sym::bits => self.write_scalar(
                    Scalar::from_u32(bit_width.try_into().expect("bit_width overflowed")),
                    &field_place,
                )?,
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }
        interp_ok(())
    }

    pub(crate) fn write_reference_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        mutability: Mutability,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Reference`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                // Write the `TypeId` of the reference's inner type to the `ty` field.
                sym::pointee => self.write_type_id(ty, &field_place)?,
                // Write the boolean representing the reference's mutability to the `mutable` field.
                sym::mutable => {
                    self.write_scalar(Scalar::from_bool(mutability.is_mut()), &field_place)?
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }
        interp_ok(())
    }

    pub(crate) fn write_pointer_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        mutability: Mutability,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Pointer`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                // Write the `TypeId` of the pointer's inner type to the `ty` field.
                sym::pointee => self.write_type_id(ty, &field_place)?,
                // Write the boolean representing the pointer's mutability to the `mutable` field.
                sym::mutable => {
                    self.write_scalar(Scalar::from_bool(mutability.is_mut()), &field_place)?
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    pub(crate) fn write_adt_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        def: ty::AdtDef<'tcx>,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Adt`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                sym::kind => {
                    if def.is_struct() {
                        let (variant, variant_place) = self.downcast_adt_kind(sym::Struct, &field_place)?;
                        let struct_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                        self.write_struct_type_info(struct_place, ty, def, args)?;
                        self.write_discriminant(variant, &field_place)?;
                    } else if def.is_enum() {
                        let (variant, variant_place) = self.downcast_adt_kind(sym::Enum, &field_place)?;
                        let enum_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                        self.write_enum_type_info(enum_place, ty, def, args)?;
                        self.write_discriminant(variant, &field_place)?;
                    } else if def.is_union() {
                        let (variant, variant_place) = self.downcast_adt_kind(sym::Union, &field_place)?;
                        let union_place = self.project_field(&variant_place, FieldIdx::ZERO)?;
                        self.write_union_type_info(union_place, ty, def, args)?;
                        self.write_discriminant(variant, &field_place)?;
                    }
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn downcast_adt_kind(
        &mut self,
        name: Symbol,
        field_dest: &impl Writeable<'tcx, CtfeProvenance>,
    ) -> InterpResult<'tcx, (ty::VariantIdx, MPlaceTy<'tcx>)> {
        let variants = field_dest.layout().ty.ty_adt_def().unwrap().variants();
        let variant_id = variants
            .iter_enumerated()
            .find(|(_idx, var)| var.name == name)
            .unwrap_or_else(|| panic!("got {name} but expected one of {variants:#?}"))
            .0;

        interp_ok((variant_id, self.project_downcast(field_dest, variant_id)?))
    }

    fn downcast_option(
        &mut self,
        name: Symbol,
        field_dest: &impl Writeable<'tcx, CtfeProvenance>,
    ) -> InterpResult<'tcx, (ty::VariantIdx, MPlaceTy<'tcx>)> {
        let variants = field_dest.layout().ty.ty_adt_def().unwrap().variants();
        let variant_id = variants
            .iter_enumerated()
            .find(|(_idx, var)| var.name == name)
            .unwrap_or_else(|| panic!("got {name} but expected one of {variants:#?}"))
            .0;

        interp_ok((variant_id, self.project_downcast(field_dest, variant_id)?))
    }

    fn write_str_slice(
        &mut self,
        place: &impl Writeable<'tcx, CtfeProvenance>,
        s: &str,
    ) -> InterpResult<'tcx> {
        // Allocate memory for the string
        let str_ty = self.tcx.tcx.types.str_;
        let str_layout = self.layout_of(str_ty)?;
        let str_place = self.allocate_dyn(str_layout, MemoryKind::Stack, s.len())?;
        
        // Write the string bytes
        self.write_bytes_ptr(str_place.ptr(), s.as_bytes().iter().copied())?;
        
        let str_place = str_place.map_provenance(CtfeProvenance::as_immutable);
        
        // Create the slice
        let ptr = Immediate::new_slice(str_place.ptr(), s.len() as u64, self);
        self.write_immediate(ptr, place)
    }

    fn write_struct_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        def: ty::AdtDef<'tcx>,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        let variant = def.non_enum_variant();

        // Iterate over all fields of `type_info::Struct`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                sym::kind => {
                    let struct_kind_variant = if variant.fields.is_empty() {
                        sym::Unit
                    } else if variant.ctor_kind() == Some(rustc_hir::def::CtorKind::Fn) {
                        sym::Tuple
                    } else {
                        sym::Named
                    };
                    let (variant_idx, _) = self.downcast_adt_kind(struct_kind_variant, &field_place)?;
                    self.write_discriminant(variant_idx, &field_place)?;
                }
                sym::fields => {
                    self.write_adt_fields(&field_place, ty, variant, args)?;
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn write_enum_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        def: ty::AdtDef<'tcx>,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Enum`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                sym::variants => {
                    self.write_enum_variants(&field_place, ty, def, args)?;
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn write_union_type_info(
        &mut self,
        place: impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        def: ty::AdtDef<'tcx>,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        let variant = def.non_enum_variant();

        // Iterate over all fields of `type_info::Union`.
        for (field_idx, field) in
            place.layout().ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                sym::fields => {
                    self.write_adt_fields(&field_place, ty, variant, args)?;
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn write_enum_variants(
        &mut self,
        variants_slice_place: &impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        def: ty::AdtDef<'tcx>,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        // get the `type_info::Variant` type from `variants: &[Variant]`
        let variant_type = variants_slice_place
            .layout()
            .ty
            .builtin_deref(false)
            .unwrap()
            .sequence_element_type(self.tcx.tcx);
        
        // Create an array with as many elements as the number of variants in the enum
        let variants_layout =
            self.layout_of(Ty::new_array(self.tcx.tcx, variant_type, def.variants().len() as u64))?;
        let variants_place = self.allocate(variants_layout, MemoryKind::Stack)?;
        let mut variants_places = self.project_array_fields(&variants_place)?;

        for variant in def.variants() {
            let Some((_, place)) = variants_places.next(self)? else {
                span_bug!(self.tcx.span, "enum variants length computed wrong");
            };
            self.write_enum_variant(place, ty, variant, args)?;
        }

        let variants_place = variants_place.map_provenance(CtfeProvenance::as_immutable);
        let ptr = Immediate::new_slice(variants_place.ptr(), def.variants().len() as u64, self);

        self.write_immediate(ptr, variants_slice_place)
    }

    fn write_enum_variant(
        &mut self,
        place: MPlaceTy<'tcx>,
        ty: Ty<'tcx>,
        variant: &ty::VariantDef,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        // Iterate over all fields of `type_info::Variant`.
        for (field_idx, field) in
            place.layout.ty.ty_adt_def().unwrap().non_enum_variant().fields.iter_enumerated()
        {
            let field_place = self.project_field(&place, field_idx)?;

            match field.name {
                sym::name => {
                    let variant_name = variant.name.as_str();
                    self.write_str_slice(&field_place, variant_name)?;
                }
                sym::kind => {
                    let variant_kind_variant = if variant.fields.is_empty() {
                        sym::Unit
                    } else if variant.ctor_kind() == Some(rustc_hir::def::CtorKind::Fn) {
                        sym::Tuple
                    } else {
                        sym::Named
                    };
                    let (variant_idx, _) = self.downcast_adt_kind(variant_kind_variant, &field_place)?;
                    self.write_discriminant(variant_idx, &field_place)?;
                }
                sym::fields => {
                    self.write_adt_fields(&field_place, ty, variant, args)?;
                }
                other => span_bug!(self.tcx.def_span(field.did), "unimplemented field {other}"),
            }
        }

        interp_ok(())
    }

    fn write_adt_fields(
        &mut self,
        fields_slice_place: &impl Writeable<'tcx, CtfeProvenance>,
        ty: Ty<'tcx>,
        variant: &ty::VariantDef,
        args: ty::GenericArgsRef<'tcx>,
    ) -> InterpResult<'tcx> {
        // get the `type_info::Field` type from `fields: &[Field]`
        let field_type = fields_slice_place
            .layout()
            .ty
            .builtin_deref(false)
            .unwrap()
            .sequence_element_type(self.tcx.tcx);

        // Create an array with as many elements as the number of fields in the struct
        let fields_layout =
            self.layout_of(Ty::new_array(self.tcx.tcx, field_type, variant.fields.len() as u64))?;
        let fields_place = self.allocate(fields_layout, MemoryKind::Stack)?;
        let mut fields_places = self.project_array_fields(&fields_place)?;

        let layout = self.layout_of(ty)?;

        while let Some((i, place)) = fields_places.next(self)? {
            let field_def = &variant.fields[i.into()];
            let field_ty = field_def.ty(self.tcx.tcx, args);
            let field_name = field_def.name.as_str();
            // Check if this is a tuple field (numeric name like "0", "1", etc.)
            let is_tuple_field = field_name.chars().all(|c| c.is_ascii_digit());
            let name = if is_tuple_field { None } else { Some(field_name) };
            self.write_field_with_name(name, field_ty, place, layout, i)?;
        }

        let fields_place = fields_place.map_provenance(CtfeProvenance::as_immutable);
        let ptr = Immediate::new_slice(fields_place.ptr(), variant.fields.len() as u64, self);

        self.write_immediate(ptr, fields_slice_place)
    }
}
